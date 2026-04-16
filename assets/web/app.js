"use strict";

// ---------- API client ----------
const api = {
  async json(method, path, body) {
    // cache: "no-store" prevents the browser from returning a stale REST
    // response after an organ switch (the server port is the same but the
    // content behind it isn't). signal aborts in-flight requests when the
    // server signals a restart, so stale replies can't overwrite fresh
    // data loaded from the new server.
    const opts = {
      method,
      headers: {},
      cache: "no-store",
      signal: wsCtrl.abortController?.signal,
    };
    if (body !== undefined) {
      opts.headers["Content-Type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    const resp = await fetch(path, opts);
    if (!resp.ok) {
      const text = await resp.text().catch(() => "");
      throw new Error(`${method} ${path} → ${resp.status} ${text}`);
    }
    const ct = resp.headers.get("content-type") || "";
    return ct.includes("json") ? resp.json() : resp.text();
  },
  organ: () => api.json("GET", "/organ"),
  stops: () => api.json("GET", "/stops"),
  setStopChannel: (stopId, ch, active) =>
    api.json("POST", `/stops/${stopId}/channels/${ch}`, { active }),
  presets: () => api.json("GET", "/presets"),
  loadPreset: (slot) => api.json("POST", `/presets/${slot}/load`),
  savePreset: (slot, name) =>
    api.json("POST", `/presets/${slot}/save`, { name }),
  panic: () => api.json("POST", "/panic"),
  audioSettings: () => api.json("GET", "/audio/settings"),
  setGain: (value) => api.json("POST", "/audio/gain", { value }),
  setPolyphony: (value) => api.json("POST", "/audio/polyphony", { value }),
  reverbs: () => api.json("GET", "/audio/reverbs"),
  selectReverb: (index) => api.json("POST", "/audio/reverbs/select", { index }),
  setReverbMix: (mix) => api.json("POST", "/audio/reverbs/mix", { mix }),
  recordMidi: (active) => api.json("POST", "/record/midi", { active }),
  recordAudio: (active) => api.json("POST", "/record/audio", { active }),
  tremulants: () => api.json("GET", "/tremulants"),
  setTremulant: (id, active) =>
    api.json("POST", `/tremulants/${encodeURIComponent(id)}`, { active }),
  midiLearnStart: (body) => api.json("POST", "/midi-learn/start", body),
  midiLearnStatus: () => api.json("GET", "/midi-learn"),
  midiLearnCancel: () => api.json("POST", "/midi-learn/cancel"),
  clearStopBinding: (stopId, ch) =>
    api.json("DELETE", `/midi-bindings/stop/${stopId}/${ch}`),
  clearTremulantBinding: (id) =>
    api.json("DELETE", `/midi-bindings/tremulant/${encodeURIComponent(id)}`),
  clearPresetBinding: (slot) =>
    api.json("DELETE", `/midi-bindings/preset/${slot}`),
  organs: () => api.json("GET", "/organs"),
  loadOrgan: (path) => api.json("POST", "/organs/load", { path }),
};

// ---------- Toasts ----------
const toastContainer = document.getElementById("toast-container");
function toast(msg, opts = {}) {
  const el = document.createElement("div");
  el.className = "toast" + (opts.error ? " error" : "");
  el.textContent = msg;
  toastContainer.appendChild(el);
  setTimeout(() => el.remove(), opts.duration ?? 2600);
}

// ---------- State ----------
const state = {
  channel: 0, // virtual MIDI channel (0-15) targeted by stop toggles
  stops: [],
  presets: [],
  tremulants: [],
  reverbs: [],
  audio: null,
};

// WebSocket lifecycle. Kept in its own object so retry timers and the
// active socket can be inspected together.
const wsCtrl = {
  ws: null,
  reconnectTimer: null,
  reconnectDelay: 500,
  openedAt: 0,
  // AbortController shared by every fetch while a WS connection is open.
  // When the server signals a restart (or the socket closes), we abort it
  // so any in-flight request can't land late with stale data from the
  // outgoing server.
  abortController: null,
};
const WS_DELAY_MAX = 3000;
const WS_DELAY_MIN = 500;
// A connection that stays open at least this long is considered "stable",
// which makes the next disconnect restart the backoff from the minimum.
const WS_STABLE_MS = 3000;

// ---------- Tabs ----------
function setupTabs() {
  const tabs = document.querySelectorAll(".tab");
  const panels = document.querySelectorAll(".tab-panel");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      const target = tab.dataset.tab;
      tabs.forEach((t) => t.setAttribute("aria-selected", t === tab));
      panels.forEach((p) =>
        p.classList.toggle("active", p.id === `tab-${target}`)
      );
      // Refresh tabs whose data could have changed since last view.
      if (target === "organs") loadOrgans().catch(() => {});
    });
  });
}

// ---------- Channel selector ----------
function setupChannelSelect() {
  const sel = document.getElementById("stop-channel-select");
  for (let i = 0; i < 16; i++) {
    const opt = document.createElement("option");
    opt.value = String(i);
    opt.textContent = `Channel ${i + 1}`;
    sel.appendChild(opt);
  }
  sel.addEventListener("change", () => {
    state.channel = Number(sel.value);
    renderStops();
  });
}

// ---------- Long-press / right-click helper ----------
function bindActivation(el, { onTap, onLong }) {
  let timer = null;
  let longFired = false;
  let startX = 0;
  let startY = 0;

  const cleanup = () => {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  };

  el.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    if (onLong) onLong(e);
  });

  el.addEventListener("pointerdown", (e) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    longFired = false;
    startX = e.clientX;
    startY = e.clientY;
    cleanup();
    timer = setTimeout(() => {
      longFired = true;
      if (onLong) onLong(e);
    }, 500);
  });

  el.addEventListener("pointermove", (e) => {
    if (!timer) return;
    if (Math.abs(e.clientX - startX) > 8 || Math.abs(e.clientY - startY) > 8) {
      cleanup();
    }
  });

  el.addEventListener("pointerup", (e) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    cleanup();
    if (!longFired && onTap) onTap(e);
  });

  el.addEventListener("pointercancel", cleanup);
  el.addEventListener("pointerleave", cleanup);
}

// ---------- Modals ----------
function openModal(id) {
  document.getElementById(id).classList.remove("hidden");
}
function closeModal(id) {
  document.getElementById(id).classList.add("hidden");
}
document.querySelectorAll("[data-modal-close]").forEach((btn) => {
  btn.addEventListener("click", () => {
    btn.closest(".modal").classList.add("hidden");
  });
});

// ---------- Organ info ----------
async function refreshOrgan() {
  try {
    const o = await api.organ();
    document.getElementById("organ-name").textContent = o.name || "Rusty Pipes";
  } catch (_) {
    // Connection state is owned by the WebSocket layer; nothing to do here.
  }
}

// ---------- Stops ----------
async function loadStops() {
  state.stops = await api.stops();
  renderStops();
}

function renderStops() {
  const container = document.getElementById("stops-container");
  container.innerHTML = "";
  // Group by division (preserving stop order)
  const groups = new Map();
  state.stops.forEach((s) => {
    const key = s.division || "—";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(s);
  });

  for (const [division, stops] of groups) {
    const section = document.createElement("section");
    section.className = "division";
    const h = document.createElement("h3");
    h.textContent = division === "—" ? "Stops" : division;
    section.appendChild(h);

    const grid = document.createElement("div");
    grid.className = "stop-grid";

    stops.forEach((stop) => {
      const tile = document.createElement("div");
      tile.className = "stop-tile";
      const isActive = stop.active_channels.includes(state.channel);
      if (isActive) tile.classList.add("active");
      tile.textContent = stop.name;
      tile.title = `${stop.name} (idx ${stop.index}, ${stop.division || "?"})`;

      bindActivation(tile, {
        onTap: () => toggleStop(stop, !isActive),
        onLong: () => openStopActions(stop),
      });

      grid.appendChild(tile);
    });

    section.appendChild(grid);
    container.appendChild(section);
  }
}

async function toggleStop(stop, active) {
  try {
    await api.setStopChannel(stop.index, state.channel, active);
    // Optimistic local update
    const set = new Set(stop.active_channels);
    if (active) set.add(state.channel);
    else set.delete(state.channel);
    stop.active_channels = [...set].sort();
    renderStops();
  } catch (e) {
    toast(`Stop toggle failed: ${e.message}`, { error: true });
  }
}

function openStopActions(stop) {
  document.getElementById("stop-actions-title").textContent = stop.name;
  document.getElementById(
    "stop-actions-subtitle"
  ).textContent = `Channel ${state.channel + 1}`;
  const enableBtn = document.getElementById("stop-action-learn-enable");
  const disableBtn = document.getElementById("stop-action-learn-disable");
  const clearBtn = document.getElementById("stop-action-clear");
  enableBtn.onclick = () => {
    closeModal("modal-stop-actions");
    startLearn({
      target: "stop",
      stop_index: stop.index,
      channel: state.channel,
      is_enable: true,
    });
  };
  disableBtn.onclick = () => {
    closeModal("modal-stop-actions");
    startLearn({
      target: "stop",
      stop_index: stop.index,
      channel: state.channel,
      is_enable: false,
    });
  };
  clearBtn.onclick = async () => {
    closeModal("modal-stop-actions");
    try {
      await api.clearStopBinding(stop.index, state.channel);
      toast(`Cleared bindings for ${stop.name} ch ${state.channel + 1}`);
    } catch (e) {
      toast(`Clear failed: ${e.message}`, { error: true });
    }
  };
  openModal("modal-stop-actions");
}

// ---------- Presets ----------
async function loadPresets() {
  state.presets = await api.presets();
  renderPresets();
}

function renderPresets() {
  const grid = document.getElementById("preset-grid");
  grid.innerHTML = "";
  state.presets.forEach((preset) => {
    const tile = document.createElement("div");
    tile.className = "preset-tile";
    if (!preset.occupied) tile.classList.add("empty");
    if (preset.is_last_loaded) tile.classList.add("active");

    const slot = document.createElement("div");
    slot.className = "slot";
    slot.textContent = `F${preset.slot}`;
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = preset.name || "(empty)";
    tile.appendChild(slot);
    tile.appendChild(name);

    bindActivation(tile, {
      onTap: () => recallPreset(preset),
      onLong: () => openPresetActions(preset),
    });
    grid.appendChild(tile);
  });
}

async function recallPreset(preset) {
  if (!preset.occupied) {
    openPresetActions(preset);
    return;
  }
  try {
    await api.loadPreset(preset.slot);
    toast(`Loaded ${preset.name || `F${preset.slot}`}`);
    await loadStops();
  } catch (e) {
    toast(`Load failed: ${e.message}`, { error: true });
  }
}

function openPresetActions(preset) {
  document.getElementById(
    "preset-actions-title"
  ).textContent = `Preset F${preset.slot}${
    preset.name ? ` — ${preset.name}` : ""
  }`;
  const loadBtn = document.getElementById("preset-action-load");
  const saveBtn = document.getElementById("preset-action-save");
  const learnBtn = document.getElementById("preset-action-learn");
  const clearBtn = document.getElementById("preset-action-clear");
  loadBtn.disabled = !preset.occupied;
  loadBtn.onclick = () => {
    closeModal("modal-preset-actions");
    recallPreset(preset);
  };
  saveBtn.onclick = () => {
    closeModal("modal-preset-actions");
    openSavePresetDialog(preset);
  };
  learnBtn.onclick = () => {
    closeModal("modal-preset-actions");
    startLearn({ target: "preset", preset_slot: preset.slot });
  };
  clearBtn.onclick = async () => {
    closeModal("modal-preset-actions");
    try {
      await api.clearPresetBinding(preset.slot);
      toast(`Cleared MIDI binding for F${preset.slot}`);
    } catch (e) {
      toast(`Clear failed: ${e.message}`, { error: true });
    }
  };
  openModal("modal-preset-actions");
}

function openSavePresetDialog(preset) {
  document.getElementById("save-preset-slot").textContent = `F${preset.slot}`;
  const input = document.getElementById("save-preset-name");
  input.value = preset.name || "";
  openModal("modal-save-preset");
  setTimeout(() => input.focus(), 50);
  document.getElementById("save-preset-confirm").onclick = async () => {
    const name = input.value.trim();
    if (!name) return;
    try {
      await api.savePreset(preset.slot, name);
      toast(`Saved as ${name}`);
      closeModal("modal-save-preset");
      loadPresets();
    } catch (e) {
      toast(`Save failed: ${e.message}`, { error: true });
    }
  };
}

// ---------- Tremulants ----------
async function loadTremulants() {
  state.tremulants = await api.tremulants();
  renderTremulants();
}

function renderTremulants() {
  const grid = document.getElementById("tremulant-grid");
  grid.innerHTML = "";
  if (state.tremulants.length === 0) {
    const p = document.createElement("p");
    p.className = "muted";
    p.textContent = "This organ has no tremulants.";
    grid.appendChild(p);
    return;
  }
  state.tremulants.forEach((trem) => {
    const tile = document.createElement("div");
    tile.className = "tremulant-tile";
    if (trem.active) tile.classList.add("active");
    tile.textContent = trem.name || trem.id;

    bindActivation(tile, {
      onTap: () => toggleTremulant(trem, !trem.active),
      onLong: () => openTremulantActions(trem),
    });
    grid.appendChild(tile);
  });
}

async function toggleTremulant(trem, active) {
  try {
    await api.setTremulant(trem.id, active);
    trem.active = active;
    renderTremulants();
  } catch (e) {
    toast(`Tremulant toggle failed: ${e.message}`, { error: true });
  }
}

function openTremulantActions(trem) {
  document.getElementById("tremulant-actions-title").textContent =
    trem.name || trem.id;
  document.getElementById("trem-action-learn-enable").onclick = () => {
    closeModal("modal-tremulant-actions");
    startLearn({
      target: "tremulant",
      tremulant_id: trem.id,
      is_enable: true,
    });
  };
  document.getElementById("trem-action-learn-disable").onclick = () => {
    closeModal("modal-tremulant-actions");
    startLearn({
      target: "tremulant",
      tremulant_id: trem.id,
      is_enable: false,
    });
  };
  document.getElementById("trem-action-clear").onclick = async () => {
    closeModal("modal-tremulant-actions");
    try {
      await api.clearTremulantBinding(trem.id);
      toast(`Cleared bindings for ${trem.name || trem.id}`);
    } catch (e) {
      toast(`Clear failed: ${e.message}`, { error: true });
    }
  };
  openModal("modal-tremulant-actions");
}

// ---------- Organs library ----------
async function loadOrgans() {
  const list = await api.organs();
  renderOrgans(list);
}

function renderOrgans(list) {
  const container = document.getElementById("organ-list");
  container.innerHTML = "";
  const currentName = document.getElementById("organ-name").textContent;
  if (!list || list.length === 0) {
    const p = document.createElement("p");
    p.className = "muted";
    p.textContent = "No organs in library.";
    container.appendChild(p);
    return;
  }
  list.forEach((entry) => {
    const item = document.createElement("div");
    item.className = "organ-item";
    if (entry.name === currentName) item.classList.add("current");

    const meta = document.createElement("div");
    meta.className = "organ-meta";
    const name = document.createElement("div");
    name.className = "name";
    name.textContent = entry.name;
    const path = document.createElement("div");
    path.className = "path";
    path.textContent = entry.path;
    meta.appendChild(name);
    meta.appendChild(path);
    item.appendChild(meta);

    if (entry.name === currentName) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = "Current";
      item.appendChild(badge);
    }

    item.addEventListener("click", () => requestLoadOrgan(entry));
    container.appendChild(item);
  });
}

async function requestLoadOrgan(entry) {
  const currentName = document.getElementById("organ-name").textContent;
  if (entry.name === currentName) {
    toast(`${entry.name} is already loaded`);
    return;
  }
  if (
    !confirm(
      `Load "${entry.name}"? The application will reload — playing notes will stop and the web UI will briefly disconnect.`,
    )
  )
    return;
  try {
    await api.loadOrgan(entry.path);
    toast(`Loading ${entry.name}…`);
  } catch (e) {
    toast(`Load failed: ${e.message}`, { error: true });
  }
}

// ---------- Audio settings ----------
async function loadAudio() {
  state.audio = await api.audioSettings();
  state.reverbs = await api.reverbs();
  renderAudio();
  renderRecording();
}

function renderAudio() {
  if (!state.audio) return;
  const a = state.audio;

  const gain = document.getElementById("gain-slider");
  const gainVal = document.getElementById("gain-value");
  gain.value = a.gain;
  gainVal.textContent = a.gain.toFixed(2);

  const poly = document.getElementById("polyphony-slider");
  const polyVal = document.getElementById("polyphony-value");
  if (a.polyphony > Number(poly.max)) poly.max = String(a.polyphony);
  poly.value = a.polyphony;
  polyVal.textContent = String(a.polyphony);

  const reverbSel = document.getElementById("reverb-select");
  reverbSel.innerHTML = "";
  const noneOpt = document.createElement("option");
  noneOpt.value = "-1";
  noneOpt.textContent = "(disabled)";
  reverbSel.appendChild(noneOpt);
  state.reverbs.forEach((r) => {
    const opt = document.createElement("option");
    opt.value = String(r.index);
    opt.textContent = r.name;
    reverbSel.appendChild(opt);
  });
  reverbSel.value = String(a.active_reverb_index ?? -1);

  const mix = document.getElementById("reverb-mix-slider");
  const mixVal = document.getElementById("reverb-mix-value");
  mix.value = a.reverb_mix;
  mixVal.textContent = a.reverb_mix.toFixed(2);
}

function setupAudioControls() {
  const gain = document.getElementById("gain-slider");
  const gainVal = document.getElementById("gain-value");
  gain.addEventListener("input", () => {
    gainVal.textContent = Number(gain.value).toFixed(2);
  });
  gain.addEventListener("change", () => {
    api.setGain(Number(gain.value)).catch((e) =>
      toast(`Gain failed: ${e.message}`, { error: true })
    );
  });

  const poly = document.getElementById("polyphony-slider");
  const polyVal = document.getElementById("polyphony-value");
  poly.addEventListener("input", () => {
    polyVal.textContent = String(poly.value);
  });
  poly.addEventListener("change", () => {
    api.setPolyphony(Number(poly.value)).catch((e) =>
      toast(`Polyphony failed: ${e.message}`, { error: true })
    );
  });

  const reverbSel = document.getElementById("reverb-select");
  reverbSel.addEventListener("change", () => {
    api.selectReverb(Number(reverbSel.value)).catch((e) =>
      toast(`Reverb failed: ${e.message}`, { error: true })
    );
  });

  const mix = document.getElementById("reverb-mix-slider");
  const mixVal = document.getElementById("reverb-mix-value");
  mix.addEventListener("input", () => {
    mixVal.textContent = Number(mix.value).toFixed(2);
  });
  mix.addEventListener("change", () => {
    api.setReverbMix(Number(mix.value)).catch((e) =>
      toast(`Mix failed: ${e.message}`, { error: true })
    );
  });
}

// ---------- Recording ----------
function renderRecording() {
  const midiBtn = document.getElementById("record-midi-btn");
  const audioBtn = document.getElementById("record-audio-btn");
  const a = state.audio;
  if (!a) return;
  midiBtn.classList.toggle("on", a.is_recording_midi);
  midiBtn.textContent = a.is_recording_midi
    ? "■ Stop MIDI Recording"
    : "● Start MIDI Recording";
  audioBtn.classList.toggle("on", a.is_recording_audio);
  audioBtn.textContent = a.is_recording_audio
    ? "■ Stop Audio Recording"
    : "● Start Audio Recording";
}

function setupRecordingControls() {
  document.getElementById("record-midi-btn").addEventListener("click", async () => {
    const newState = !(state.audio?.is_recording_midi);
    try {
      await api.recordMidi(newState);
      state.audio.is_recording_midi = newState;
      renderRecording();
      toast(newState ? "MIDI recording started" : "MIDI recording saved");
    } catch (e) {
      toast(`Recording failed: ${e.message}`, { error: true });
    }
  });
  document.getElementById("record-audio-btn").addEventListener("click", async () => {
    const newState = !(state.audio?.is_recording_audio);
    try {
      await api.recordAudio(newState);
      state.audio.is_recording_audio = newState;
      renderRecording();
      toast(newState ? "Audio recording started" : "Audio recording saved");
    } catch (e) {
      toast(`Recording failed: ${e.message}`, { error: true });
    }
  });
}

// ---------- Panic ----------
document.getElementById("panic-btn").addEventListener("click", async () => {
  try {
    await api.panic();
    toast("Panic — all notes off");
  } catch (e) {
    toast(`Panic failed: ${e.message}`, { error: true });
  }
});

// ---------- MIDI Learn ----------
// State transitions arrive over the WebSocket; this module just opens the
// modal on start and closes it when the server announces a transition.
let learnAutoCloseHandle = null;
let learnActive = false;

async function startLearn(targetBody) {
  try {
    const resp = await api.midiLearnStart(targetBody);
    document.getElementById("learn-target-label").textContent =
      resp.target_name || "(target)";
    document.getElementById("learn-state-label").textContent =
      "Waiting for MIDI event…";
    document.getElementById("learn-result").textContent = "";
    if (learnAutoCloseHandle) {
      clearTimeout(learnAutoCloseHandle);
      learnAutoCloseHandle = null;
    }
    learnActive = true;
    openModal("modal-learn");
  } catch (e) {
    toast(`Learn failed: ${e.message}`, { error: true });
  }
}

function handleLearnUpdate(msg) {
  if (!learnActive) return;
  if (msg.state === "captured") {
    document.getElementById("learn-state-label").textContent = "Learned ✓";
    document.getElementById("learn-result").textContent =
      msg.event_description || "";
    toast(`Learned: ${msg.event_description || "event"}`);
    learnActive = false;
    learnAutoCloseHandle = setTimeout(() => closeModal("modal-learn"), 1100);
  } else if (msg.state === "timed_out") {
    document.getElementById("learn-state-label").textContent =
      "Timed out — no MIDI event received.";
    learnActive = false;
    learnAutoCloseHandle = setTimeout(() => closeModal("modal-learn"), 1500);
  } else if (msg.state === "idle") {
    learnActive = false;
    closeModal("modal-learn");
  }
}

document.getElementById("learn-cancel").addEventListener("click", async () => {
  learnActive = false;
  if (learnAutoCloseHandle) {
    clearTimeout(learnAutoCloseHandle);
    learnAutoCloseHandle = null;
  }
  try {
    await api.midiLearnCancel();
  } catch (_) {}
  closeModal("modal-learn");
});

// ---------- WebSocket ----------
// State -> CSS class on the topbar status dot. The visible label is moved
// to the title attribute so hover/long-press still surfaces details.
function setStatus(state, label) {
  const el = document.getElementById("status-dot");
  if (!el) return;
  el.classList.remove("connected", "connecting", "reconnecting");
  el.classList.add(state);
  el.title = label;
}

function scheduleReconnect() {
  if (wsCtrl.reconnectTimer) return; // single-flight
  const secs = Math.ceil(wsCtrl.reconnectDelay / 1000);
  setStatus("reconnecting", `Reconnecting in ${secs}s…`);
  wsCtrl.reconnectTimer = setTimeout(() => {
    wsCtrl.reconnectTimer = null;
    connectWebSocket();
  }, wsCtrl.reconnectDelay);
  wsCtrl.reconnectDelay = Math.min(
    wsCtrl.reconnectDelay * 2,
    WS_DELAY_MAX,
  );
}

function reconnectNow() {
  // Used by online / visibilitychange / manual prompts to skip the backoff.
  if (wsCtrl.reconnectTimer) {
    clearTimeout(wsCtrl.reconnectTimer);
    wsCtrl.reconnectTimer = null;
  }
  if (wsCtrl.ws && wsCtrl.ws.readyState === WebSocket.OPEN) return;
  wsCtrl.reconnectDelay = WS_DELAY_MIN;
  connectWebSocket();
}

function connectWebSocket() {
  // Tear down any existing socket — important when reconnectNow() races with
  // an in-flight CONNECTING socket from a previous attempt.
  if (wsCtrl.ws) {
    try {
      wsCtrl.ws.onopen = wsCtrl.ws.onmessage = wsCtrl.ws.onerror = wsCtrl.ws.onclose = null;
      wsCtrl.ws.close();
    } catch (_) {}
    wsCtrl.ws = null;
  }

  let ws;
  try {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    ws = new WebSocket(`${proto}//${location.host}/ws`);
  } catch (_) {
    // Synchronous failure (rare — e.g. CSP violation, page unloading).
    scheduleReconnect();
    return;
  }
  wsCtrl.ws = ws;
  setStatus("connecting", "Connecting…");

  ws.addEventListener("open", () => {
    if (ws !== wsCtrl.ws) return; // event from a stale socket
    wsCtrl.openedAt = Date.now();
    wsCtrl.reconnectDelay = WS_DELAY_MIN;
    wsCtrl.abortController = new AbortController();
    setStatus("connected", "Connected");
    // No refetch here: the server sends a "Refetch" message as its first
    // frame to every new connection, which the message handler uses to
    // reload all state. Keeping this logic server-driven means the new
    // server is the one that decides when the client's view is fresh.
  });

  ws.addEventListener("message", (ev) => {
    if (ws !== wsCtrl.ws) return;
    let msg;
    try {
      msg = JSON.parse(ev.data);
    } catch (_) {
      return;
    }
    handleWsMessage(msg);
  });

  ws.addEventListener("close", () => {
    if (ws !== wsCtrl.ws) return;
    wsCtrl.ws = null;
    // Abort any fetches still in flight from the outgoing connection so
    // their responses can't land after the new socket opens.
    if (wsCtrl.abortController) {
      try {
        wsCtrl.abortController.abort();
      } catch (_) {}
      wsCtrl.abortController = null;
    }
    // If we were stably connected for a while, reset backoff so the next
    // outage doesn't carry over a long delay from the original failure.
    if (wsCtrl.openedAt && Date.now() - wsCtrl.openedAt > WS_STABLE_MS) {
      wsCtrl.reconnectDelay = WS_DELAY_MIN;
    }
    wsCtrl.openedAt = 0;
    scheduleReconnect();
  });

  ws.addEventListener("error", () => {
    // No-op: the close event always follows and is the single place that
    // schedules a reconnect. Calling close() here can race with the browser
    // and double-fire close in some implementations.
  });
}

// Recover quickly when the browser tells us network/page came back.
window.addEventListener("online", reconnectNow);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") reconnectNow();
});

function handleWsMessage(msg) {
  switch (msg.type) {
    case "Refetch":
      refetchAll();
      break;
    case "StopsChanged":
      loadStops().catch(() => {});
      break;
    case "PresetsChanged":
      loadPresets().catch(() => {});
      break;
    case "TremulantsChanged":
      loadTremulants().catch(() => {});
      break;
    case "AudioChanged":
      api
        .audioSettings()
        .then((a) => {
          state.audio = a;
          renderAudio();
          renderRecording();
        })
        .catch(() => {});
      break;
    case "ServerRestarting":
      // Outgoing server warned us it's about to shut down. Abort any
      // pending fetches so none of their responses can land after we
      // reconnect to the new server, then close our socket proactively.
      // The existing reconnect loop takes over from there; the new
      // server's Refetch message will drive the state reload.
      toast("Loading organ…");
      if (wsCtrl.abortController) {
        try {
          wsCtrl.abortController.abort();
        } catch (_) {}
        wsCtrl.abortController = null;
      }
      if (wsCtrl.ws) {
        try {
          wsCtrl.ws.close();
        } catch (_) {}
      }
      break;
    case "MidiLearn":
      handleLearnUpdate(msg);
      break;
  }
}

function refetchAll() {
  Promise.allSettled([
    refreshOrgan().then(() => loadOrgans().catch(() => {})),
    loadStops(),
    loadPresets(),
    loadTremulants(),
    loadAudio(),
  ]);
}

// ---------- Init ----------
async function init() {
  setupTabs();
  setupChannelSelect();
  setupAudioControls();
  setupRecordingControls();

  await refreshOrgan();
  // Best-effort initial loads — failures show up in the connection indicator.
  await Promise.allSettled([
    loadStops(),
    loadPresets(),
    loadTremulants(),
    loadAudio(),
  ]);
  connectWebSocket();
}

init();
