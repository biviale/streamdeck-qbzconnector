//! Signing in to Qobuz, and keeping the token somewhere sensible.
//!
//! The plugin holds its own Qobuz session rather than borrowing QBZ's, which
//! is encrypted at rest with a key derived from QBZ's own salt file.
//!
//! The token goes into the system keyring — on a KDE session that is KWallet
//! via `org.freedesktop.secrets`, unlocked at login. See [`store`] for what
//! happens when no keyring is available.
//!
//! The browser flow is derived from QBZ's `crates/qbzd/src/login.rs` (MIT).

use std::io::{Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{bundle, API_BASE};

const KEYRING_SERVICE: &str = "com.biviale.qbzconnector";
const KEYRING_USER: &str = "qobuz-token";

/// How long the user has to finish signing in before the listener gives up.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// The non-secret half of a session, safe to keep in the global settings.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Identity {
	pub authenticated: bool,
	pub user_id: u64,
	pub display_name: String,
	/// Index into the bundle's secret list that last signed a call
	/// successfully, so the working secret is found once rather than probed
	/// on every request.
	pub secret_index: usize,
}

/// Token persistence.
///
/// The keyring is the real answer. The file fallback exists for sessions with
/// no Secret Service provider, and it is **obfuscation, not protection**: the
/// pad sits next to the ciphertext, readable by anything running as the user.
/// It keeps a plain-text token out of a backup, and does nothing else.
pub mod store {

	use std::io::Write;
	use std::path::PathBuf;

	use super::{KEYRING_SERVICE, KEYRING_USER};

	fn fallback_path() -> Option<PathBuf> {
		Some(dirs::config_dir()?.join("qbzconnector").join("token"))
	}

	/// Obfuscate with a per-install random pad, so the token is not sitting
	/// in a file as readable text.
	fn pad(len: usize) -> Option<Vec<u8>> {
		use rand::RngExt as _;

		let path = fallback_path()?.with_file_name("token.key");
		let existing = std::fs::read(&path).ok().filter(|k| k.len() >= len);
		if let Some(key) = existing {
			return Some(key);
		}

		let mut key = vec![0u8; len.max(256)];
		rand::rng().fill(&mut key[..]);
		write_private(&path, &key).ok()?;
		Some(key)
	}

	/// Write with an owner-only mode, creating parents as needed.
	fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		let mut options = std::fs::OpenOptions::new();
		options.write(true).create(true).truncate(true);
		#[cfg(unix)]
		{
			use std::os::unix::fs::OpenOptionsExt as _;
			options.mode(0o600);
		}
		let mut file = options.open(path)?;
		file.write_all(bytes)
	}

	fn xor(bytes: &[u8], key: &[u8]) -> Vec<u8> {
		bytes.iter().zip(key.iter().cycle()).map(|(b, k)| b ^ k).collect()
	}

	/// Run a keyring call somewhere it is allowed to block.
	///
	/// The Secret Service backend drives zbus with a runtime of its own, and
	/// tokio panics — "Cannot start a runtime from within a runtime" — if
	/// that happens on a thread already driving async tasks. The blocking
	/// pool is where such work belongs.
	async fn off_the_reactor<T, F>(work: F) -> Option<T>
	where
		F: FnOnce() -> T + Send + 'static,
		T: Send + 'static,
	{
		tokio::task::spawn_blocking(work).await.ok()
	}

	pub(super) async fn keyring_get(service: &'static str, user: &'static str) -> Option<String> {
		off_the_reactor(move || {
			keyring::Entry::new(service, user)
				.ok()
				.and_then(|entry| entry.get_password().ok())
		})
		.await
		.flatten()
	}

	pub(super) async fn keyring_set(service: &'static str, user: &'static str, secret: String) -> bool {
		off_the_reactor(move || {
			keyring::Entry::new(service, user)
				.ok()
				.is_some_and(|entry| entry.set_password(&secret).is_ok())
		})
		.await
		.unwrap_or(false)
	}

	pub(super) async fn keyring_delete(service: &'static str, user: &'static str) {
		off_the_reactor(move || {
			if let Ok(entry) = keyring::Entry::new(service, user) {
				let _ = entry.delete_credential();
			}
		})
		.await;
	}

	pub async fn load() -> Option<String> {
		// The keyring first, always.
		if let Some(token) = keyring_get(KEYRING_SERVICE, KEYRING_USER).await {
			return Some(token);
		}

		let raw = std::fs::read(fallback_path()?).ok()?;
		let key = pad(raw.len())?;
		String::from_utf8(xor(&raw, &key)).ok()
	}

	pub async fn save(token: String) -> Result<(), String> {
		if keyring_set(KEYRING_SERVICE, KEYRING_USER, token.clone()).await {
			log::info!("Qobuz token stored in the system keyring");
			return Ok(());
		}

		log::warn!(
			"no system keyring available; falling back to an obfuscated file, \
			 which does not protect the token from anything running as you"
		);
		let path = fallback_path().ok_or("no config directory")?;
		let key = pad(token.len().max(256)).ok_or("cannot create key file")?;
		write_private(&path, &xor(token.as_bytes(), &key)).map_err(|e| e.to_string())
	}

	pub async fn clear() {
		keyring_delete(KEYRING_SERVICE, KEYRING_USER).await;
		if let Some(path) = fallback_path() {
			let _ = std::fs::remove_file(&path);
			let _ = std::fs::remove_file(path.with_file_name("token.key"));
		}
	}
}

/// A signed-in Qobuz session.
#[derive(Clone, Debug)]
pub struct Session {
	pub token: String,
	pub identity: Identity,
}

/// Load a previously stored session, if there is one.
pub async fn restore(identity: &Identity) -> Option<Session> {
	if !identity.authenticated {
		return None;
	}
	store::load().await.map(|token| Session {
		token,
		identity: identity.clone(),
	})
}

/// Run the browser sign-in flow end to end.
///
/// Opens the Qobuz sign-in page through the server, so it lands in the user's
/// real browser, catches the redirect on a throwaway loopback listener, and
/// exchanges the code for a token.
pub async fn login() -> Result<Session, String> {
	let bundle = bundle::load()?;
	let private_key = bundle
		.private_key
		.clone()
		.ok_or("the cached Qobuz bundle has no OAuth key; update QBZ and sign in again")?;

	// Port 0: the kernel picks a free one. Never a fixed port, so two
	// sign-in attempts cannot collide.
	let listener = std::net::TcpListener::bind("127.0.0.1:0")
		.map_err(|error| format!("cannot open a callback port: {error}"))?;
	let port = listener.local_addr().map_err(|e| e.to_string())?.port();

	// The nonce rides the redirect *path*, which Qobuz preserves verbatim.
	// A callback that does not carry it is not ours and is ignored.
	let nonce = nonce();
	let redirect = format!("http://localhost:{port}/{nonce}");
	let url = format!(
		"https://www.qobuz.com/signin/oauth?ext_app_id={}&redirect_url={}",
		bundle.app_id,
		urlencoding::encode(&redirect)
	);

	openaction::open_url(url)
		.await
		.map_err(|error| format!("could not open the browser: {error}"))?;

	let code = tokio::task::spawn_blocking(move || capture_code(listener, &nonce))
		.await
		.map_err(|error| format!("callback task failed: {error}"))??;

	let callback_token = exchange_code(&bundle.app_id, &private_key, &code).await?;
	let (identity, session_token) = fetch_identity(&bundle.app_id, &callback_token).await?;

	// Prefer the token `/user/login` returned; fall back to the one that got
	// us through the door.
	let token = session_token.unwrap_or(callback_token);
	store::save(token.clone()).await?;
	Ok(Session { token, identity })
}

/// A URL-safe random string.
fn nonce() -> String {
	use rand::RngExt as _;

	const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
	let mut rng = rand::rng();
	(0..24)
		.map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
		.collect()
}

/// Wait for the browser to hit the redirect, and pull the code out of it.
fn capture_code(listener: std::net::TcpListener, nonce: &str) -> Result<String, String> {
	// Non-blocking accept so the deadline is honoured even if the browser
	// never comes back. A blocking accept would pin this thread for the life
	// of the process once the user gave up on signing in.
	listener
		.set_nonblocking(true)
		.map_err(|error| error.to_string())?;

	let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;
	while std::time::Instant::now() < deadline {
		let mut stream = match listener.accept() {
			Ok((stream, _)) => stream,
			Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
				std::thread::sleep(Duration::from_millis(100));
				continue;
			}
			Err(error) => {
				log::debug!("callback listener: {error}");
				continue;
			}
		};
		// Accepted sockets do not inherit the listener's mode everywhere.
		let _ = stream.set_nonblocking(false);
		let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

		let mut buffer = [0u8; 4096];
		let Ok(read) = stream.read(&mut buffer) else {
			continue;
		};
		let request = String::from_utf8_lossy(&buffer[..read]);
		let Some(line) = request.lines().next() else {
			continue;
		};

		match parse_callback(line, nonce) {
			Some(code) => {
				respond(&mut stream, "Signed in. You can close this tab.");
				return Ok(code);
			}
			None => {
				// Browsers fetch /favicon.ico against the same origin, which
				// must not end the wait. Logging what arrived keeps a callback
				// of an unexpected shape visible instead of silent.
				log::info!("ignoring callback request: {}", describe_request(line));
				respond(&mut stream, "Waiting for the Qobuz sign-in…");
			}
		}
	}
	Err("sign-in timed out".to_string())
}

/// A request line with its parameter *values* stripped.
///
/// The authorization code is a credential, so the line itself must never
/// reach a log file. The path and the parameter names are what diagnosing a
/// rejected callback actually needs.
fn describe_request(request_line: &str) -> String {
	let Some(target) = request_line.split_whitespace().nth(1) else {
		return "<malformed>".to_string();
	};
	match target.split_once('?') {
		Some((path, query)) => {
			let keys: Vec<&str> = query
				.split('&')
				.map(|pair| pair.split_once('=').map(|(key, _)| key).unwrap_or(pair))
				.collect();
			format!("{path} with [{}]", keys.join(", "))
		}
		None => format!("{target} with no query"),
	}
}

/// Pull the authorization code out of `GET /<nonce>?... HTTP/1.1`.
///
/// The path must carry our nonce: that is what ties the callback to the
/// request we started, so another page cannot feed us a code.
fn parse_callback(request_line: &str, nonce: &str) -> Option<String> {
	let mut parts = request_line.split_whitespace();
	if parts.next()? != "GET" {
		return None;
	}
	let target = parts.next()?;
	let (path, query) = target.split_once('?')?;
	// Trim both ends: a browser may add a trailing slash to the redirect.
	if path.trim_matches('/') != nonce {
		return None;
	}
	code_from_query(query)
}

/// The code, under whichever name Qobuz used.
///
/// Qobuz names the parameter `code_autorisation`; `code` is kept as a
/// fallback for the OAuth convention. QBZ does the same, in the same order.
fn code_from_query(query: &str) -> Option<String> {
	let mut qobuz_name = None;
	let mut conventional = None;

	for pair in query.split('&') {
		let Some((key, value)) = pair.split_once('=') else {
			continue;
		};
		// The code rides in a URL, so it can arrive percent-escaped.
		let decoded = urlencoding::decode(value).ok().map(|s| s.into_owned());
		match key {
			"code_autorisation" => qobuz_name = decoded,
			"code" => conventional = decoded,
			_ => {}
		}
	}

	qobuz_name.or(conventional)
}

fn respond(stream: &mut std::net::TcpStream, message: &str) {
	let body = format!(
		"<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
		 <title>QBZ Connector</title></head><body style=\"font:16px system-ui;padding:3rem\">\
		 <p>{message}</p></body></html>"
	);
	let response = format!(
		"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
		 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
		body.len()
	);
	let _ = stream.write_all(response.as_bytes());
	let _ = stream.flush();
}

/// Trade the authorization code for a user auth token.
async fn exchange_code(app_id: &str, private_key: &str, code: &str) -> Result<String, String> {
	let response = super::http_client()
		.map_err(|e| e.to_string())?
		.get(format!("{API_BASE}/oauth/callback"))
		.header("X-App-Id", app_id)
		.query(&[("code", code), ("private_key", private_key), ("app_id", app_id)])
		.send()
		.await
		.map_err(|error| format!("OAuth exchange failed: {error}"))?;

	if !response.status().is_success() {
		return Err(format!("OAuth exchange refused ({})", response.status()));
	}
	let json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
	token_from_callback(&json).ok_or_else(|| "OAuth exchange returned no token".to_string())
}

/// The token in an `/oauth/callback` response.
///
/// The field is `token`. `user_auth_token` is the *next* step's field, in the
/// `/user/login` response: reading that one here fails the sign-in after the
/// user has already authenticated.
fn token_from_callback(response: &serde_json::Value) -> Option<String> {
	response
		.get("token")
		.and_then(|v| v.as_str())
		.map(|s| s.to_string())
}

/// Confirm the token works and find out who it belongs to.
///
/// Returns the identity plus the session token Qobuz wants used from here on.
async fn fetch_identity(app_id: &str, token: &str) -> Result<(Identity, Option<String>), String> {
	let response = super::http_client()
		.map_err(|e| e.to_string())?
		.post(format!("{API_BASE}/user/login"))
		.header("X-App-Id", app_id)
		.header("X-User-Auth-Token", token)
		.header("Content-Type", "text/plain;charset=UTF-8")
		.body("extra=partner")
		.send()
		.await
		.map_err(|error| format!("could not confirm the session: {error}"))?;

	if !response.status().is_success() {
		return Err(format!("Qobuz rejected the session ({})", response.status()));
	}
	let json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
	identity_from_login(&json).ok_or_else(|| "no user in the login response".to_string())
}

/// The account identity, and the session token `/user/login` hands back.
///
/// The login response carries its own `user_auth_token`; that is the one to
/// keep, the callback token having served its purpose. It is optional here so
/// a response that omits it falls back to the callback token rather than
/// failing a sign-in that otherwise worked.
fn identity_from_login(response: &serde_json::Value) -> Option<(Identity, Option<String>)> {
	let user = response.get("user")?;

	let identity = Identity {
		authenticated: true,
		user_id: user.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
		display_name: user
			.get("display_name")
			.and_then(|v| v.as_str())
			.or_else(|| user.get("login").and_then(|v| v.as_str()))
			.unwrap_or("")
			.to_string(),
		secret_index: 0,
	};

	let token = response
		.get("user_auth_token")
		.and_then(|v| v.as_str())
		.map(|s| s.to_string());

	Some((identity, token))
}

#[cfg(test)]
mod tests {
	use super::parse_callback;

	#[test]
	fn accepts_a_callback_carrying_the_nonce() {
		let code = parse_callback("GET /abc123?code=THECODE&x=1 HTTP/1.1", "abc123");
		assert_eq!(code.as_deref(), Some("THECODE"));
	}

	/// What Qobuz actually sends. Reject this name and the callback arrives,
	/// goes unrecognised, and the browser waits forever.
	#[test]
	fn accepts_the_parameter_name_qobuz_really_uses() {
		let code = parse_callback("GET /abc123?code_autorisation=THECODE HTTP/1.1", "abc123");
		assert_eq!(code.as_deref(), Some("THECODE"));
	}

	#[test]
	fn the_qobuz_parameter_wins_over_a_plain_code() {
		let code = parse_callback("GET /abc123?code=OTHER&code_autorisation=REAL HTTP/1.1", "abc123");
		assert_eq!(code.as_deref(), Some("REAL"));
	}

	/// The code travels through a URL, so it can arrive escaped; the raw form
	/// fails the token exchange.
	#[test]
	fn percent_escapes_in_the_code_are_decoded() {
		let code = parse_callback("GET /abc123?code_autorisation=x%2Fy HTTP/1.1", "abc123");
		assert_eq!(code.as_deref(), Some("x/y"));
	}

	#[test]
	fn a_trailing_slash_on_the_path_is_tolerated() {
		let code = parse_callback("GET /abc123/?code_autorisation=THECODE HTTP/1.1", "abc123");
		assert_eq!(code.as_deref(), Some("THECODE"));
	}

	#[test]
	fn rejects_a_callback_with_the_wrong_nonce() {
		assert!(parse_callback("GET /wrong?code=THECODE HTTP/1.1", "abc123").is_none());
	}

	/// Two responses, two field names: reading the second step's name out of
	/// the first step's response fails a sign-in the browser already passed.
	#[test]
	fn the_callback_token_comes_from_the_token_field() {
		let response = serde_json::json!({ "token": "THE_TOKEN" });
		assert_eq!(
			super::token_from_callback(&response).as_deref(),
			Some("THE_TOKEN")
		);
	}

	#[test]
	fn a_callback_response_without_a_token_is_rejected() {
		let response = serde_json::json!({ "status": "ok" });
		assert!(super::token_from_callback(&response).is_none());
	}

	#[test]
	fn the_login_response_yields_the_account_identity() {
		let response = serde_json::json!({
			"user_auth_token": "SESSION_TOKEN",
			"user": { "id": 12345678, "display_name": "someone", "login": "fallback" }
		});
		let (identity, token) = super::identity_from_login(&response).unwrap();
		assert!(identity.authenticated);
		assert_eq!(identity.user_id, 12345678);
		assert_eq!(identity.display_name, "someone");
		assert_eq!(token.as_deref(), Some("SESSION_TOKEN"));
	}

	/// Not every account has a display name set.
	#[test]
	fn the_login_name_falls_back_to_the_login_field() {
		let response = serde_json::json!({ "user": { "id": 1, "login": "fallback" } });
		let (identity, _) = super::identity_from_login(&response).unwrap();
		assert_eq!(identity.display_name, "fallback");
	}

	#[test]
	fn a_login_response_with_no_user_is_rejected() {
		assert!(super::identity_from_login(&serde_json::json!({})).is_none());
	}

	/// The code is a credential, so a rejected callback has to be describable
	/// without writing it to a log file.
	#[test]
	fn a_described_request_keeps_the_names_and_drops_the_values() {
		let described = super::describe_request("GET /abc123?code_autorisation=SECRET&state=xyz HTTP/1.1");
		assert!(described.contains("code_autorisation"), "{described}");
		assert!(described.contains("state"), "{described}");
		assert!(!described.contains("SECRET"), "{described}");
		assert!(!described.contains("xyz"), "{described}");
	}

	#[test]
	fn ignores_the_browsers_favicon_request() {
		assert!(parse_callback("GET /favicon.ico HTTP/1.1", "abc123").is_none());
	}
}

#[cfg(test)]
mod keyring_tests {
	use super::store::{keyring_delete, keyring_get, keyring_set};

	/// Needs a Secret Service provider, and writes to the real keyring under
	/// a probe name it cleans up afterwards.
	///
	/// The keyring backend starts a runtime of its own, so calling it straight
	/// from async code aborts the task, at the very last step of sign-in.
	#[tokio::test]
	#[ignore]
	async fn the_keyring_is_usable_from_async_code() {
		const SERVICE: &str = "com.biviale.qbzconnector.test";
		const USER: &str = "probe";

		keyring_delete(SERVICE, USER).await;
		assert!(keyring_get(SERVICE, USER).await.is_none(), "probe not clean");

		assert!(
			keyring_set(SERVICE, USER, "secret-value".to_string()).await,
			"could not write to the keyring"
		);
		assert_eq!(keyring_get(SERVICE, USER).await.as_deref(), Some("secret-value"));

		keyring_delete(SERVICE, USER).await;
		assert!(keyring_get(SERVICE, USER).await.is_none());
	}
}
