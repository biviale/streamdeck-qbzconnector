// Minimal OpenAction property-inspector client.
//
// Deliberately dependency-free rather than using sdpi-components: that ships
// from a CDN, and a property inspector that needs the network to render its
// own form is a poor trade for a handful of fields.
//
// Usage: give a form control `data-setting="name"` for a setting that belongs
// to this key, or `data-global="name"` for one shared by the whole plugin.
// Either way the value is read from and written back automatically.

(function () {
	let socket = null;
	let context = null;
	let actionUuid = null;
	let settings = {};
	let globals = {};
	const settingsListeners = [];
	const pluginListeners = [];

	function send(payload) {
		if (socket && socket.readyState === WebSocket.OPEN) {
			socket.send(JSON.stringify(payload));
		}
	}

	/** Push the current settings to the plugin. */
	function save() {
		send({ event: "setSettings", context, payload: settings });
	}

	/** Push the plugin-wide settings. */
	function saveGlobals() {
		send({ event: "setGlobalSettings", context, payload: globals });
	}

	/** Read a control's value in the type its setting expects. */
	function readControl(el) {
		if (el.type === "checkbox") return el.checked;
		if (el.type === "number" || el.type === "range") {
			const n = parseFloat(el.value);
			return Number.isNaN(n) ? null : n;
		}
		return el.value;
	}

	function writeControl(el, value) {
		if (value === undefined || value === null) return;
		if (el.type === "checkbox") el.checked = Boolean(value);
		else el.value = value;
	}

	/** Mirror the settings object onto every bound control. */
	function applySettings() {
		document.querySelectorAll("[data-setting]").forEach((el) => {
			writeControl(el, settings[el.dataset.setting]);
		});
		settingsListeners.forEach((cb) => cb(settings));
	}

	function applyGlobals() {
		document.querySelectorAll("[data-global]").forEach((el) => {
			writeControl(el, globals[el.dataset.global]);
		});
	}

	/** Which DOM event means "the user is done with this control". */
	function commitEvent(el) {
		// `change` rather than `input` for text and number fields, so a
		// half-typed value never reaches the plugin.
		return el.type === "range" || el.tagName === "SELECT" || el.type === "checkbox"
			? "input"
			: "change";
	}

	function bindControls() {
		document.querySelectorAll("[data-setting]").forEach((el) => {
			el.addEventListener(commitEvent(el), () => {
				const value = readControl(el);
				if (value === null) return;
				settings[el.dataset.setting] = value;
				save();
				settingsListeners.forEach((cb) => cb(settings));
			});
		});

		document.querySelectorAll("[data-global]").forEach((el) => {
			el.addEventListener(commitEvent(el), () => {
				const value = readControl(el);
				if (value === null) return;
				globals[el.dataset.global] = value;
				saveGlobals();
			});
		});
	}

	window.PI = {
		get settings() {
			return settings;
		},
		save,
		/** Called once at connect time and on every settings change. */
		onSettings(cb) {
			settingsListeners.push(cb);
			if (context) cb(settings);
		},
		/** Send an arbitrary payload to the plugin. */
		sendToPlugin(payload) {
			send({ event: "sendToPlugin", action: actionUuid, context, payload });
		},
		onFromPlugin(cb) {
			pluginListeners.push(cb);
		},
	};

	// OpenDeck calls this once the inspector frame is ready. `actionInfo`
	// carries the action's current settings, so the form can be filled in
	// without a round trip.
	window.connectOpenActionSocket = function (port, uuid, registerEvent, _info, actionInfo) {
		context = uuid;
		try {
			const parsed = JSON.parse(actionInfo);
			actionUuid = parsed.action;
			settings = parsed.payload?.settings ?? {};
		} catch (_) {
			settings = {};
		}

		socket = new WebSocket("ws://localhost:" + port);
		socket.onopen = () => {
			send({ event: registerEvent, uuid });
			bindControls();
			applySettings();
			// Plugin-wide settings are not in actionInfo, so they have to be
			// asked for; the reply arrives as didReceiveGlobalSettings.
			send({ event: "getGlobalSettings", context });
		};
		socket.onmessage = (message) => {
			let data;
			try {
				data = JSON.parse(message.data);
			} catch (_) {
				return;
			}
			if (data.event === "didReceiveSettings") {
				settings = data.payload?.settings ?? {};
				applySettings();
			} else if (data.event === "didReceiveGlobalSettings") {
				globals = data.payload?.settings ?? {};
				applyGlobals();
			} else if (data.event === "sendToPropertyInspector") {
				pluginListeners.forEach((cb) => cb(data.payload));
			}
		};
	};

	// Stream Deck compatibility.
	window.connectElgatoStreamDeckSocket = window.connectOpenActionSocket;
})();
