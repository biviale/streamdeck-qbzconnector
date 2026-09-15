//! OpenAction plugin for the QBZ Qobuz desktop client.
//!
//! See README.md for what QBZ does and does not let a remote control do.

mod actions;
mod glyphs;
mod hub;
mod mpris;
mod palette;
mod qobuz;
mod render;
mod settings;
mod volume;

use openaction::*;

struct Plugin;

#[async_trait]
impl global_events::GlobalEventHandler for Plugin {
	/// Called once the WebSocket handshake with the server has completed.
	/// Starting the hub here rather than in `main` means the first state
	/// snapshot can already reach keys that are on screen.
	async fn plugin_ready(&self) -> OpenActionResult<()> {
		if let Err(error) = hub::start().await {
			log::error!("could not connect to the session bus: {error}");
			return Ok(());
		}

		volume::start().await;

		let mut states = hub::hub().subscribe();
		tokio::spawn(async move {
			// `changed()` skips the value present at subscribe time, which is
			// the empty default; `will_appear` renders that one.
			while states.changed().await.is_ok() {
				let snapshot = states.borrow_and_update().clone();
				actions::refresh_all(&snapshot).await;
			}
		});

		// The Qobuz identity lives in global settings; the reply arrives as
		// `did_receive_global_settings` below.
		if let Err(error) = get_global_settings().await {
			log::warn!("could not request global settings: {error}");
		}

		Ok(())
	}

	async fn did_receive_global_settings(
		&self,
		event: global_events::DidReceiveGlobalSettingsEvent,
	) -> OpenActionResult<()> {
		let global = settings::adopt(event.payload.settings);

		// The tint may have just been turned on, or restored at startup.
		hub::refresh_accent();

		if !global.identity.authenticated {
			return Ok(());
		}

		// Restoring hits the network to warm the favourites cache, so it runs
		// detached rather than holding up the handshake.
		tokio::spawn(async move {
			qobuz::library::restore(&global.identity).await;
			actions::favorite::refresh(&hub::hub().state()).await;
		});
		Ok(())
	}
}

#[tokio::main]
async fn main() -> OpenActionResult<()> {
	{
		use simplelog::*;
		// The server captures the plugin's stdout; OpenDeck files it under
		// ~/.local/share/opendeck/logs/plugins/<uuid>.log.
		if let Err(error) = TermLogger::init(
			LevelFilter::Info,
			Config::default(),
			TerminalMode::Stdout,
			ColorChoice::Never,
		) {
			eprintln!("logger initialisation failed: {error}");
		}
	}

	global_events::set_global_event_handler(&Plugin);

	register_action(actions::transport::PlayPause).await;
	register_action(actions::transport::Next).await;
	register_action(actions::transport::Previous).await;
	register_action(actions::transport::Stop).await;
	register_action(actions::transport::SeekForward).await;
	register_action(actions::transport::SeekBackward).await;

	register_action(actions::volume::VolumeUp).await;
	register_action(actions::volume::VolumeDown).await;
	register_action(actions::volume::VolumeSet).await;
	register_action(actions::volume::Mute).await;
	register_action(actions::volume::VolumeDial).await;

	register_action(actions::nowplaying::NowPlaying).await;
	register_action(actions::favorite::Favorite).await;

	run(std::env::args().collect()).await
}
