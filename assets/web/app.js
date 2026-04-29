"use strict";

// ---------- Translations ----------
// Populated from GET /i18n on startup and after each WS reconnect (the
// server might be a different one — config vs play — but both expose the
// same dictionary). Falls back to the English text in the HTML / JS source.
const i18n = {
  locale: "en",
  strings: {},
  languages: [],
};

// Translate a key, optionally substituting `%{name}` placeholders. Falls
// back to the key itself when not found.
function t(key, params) {
  const tmpl = i18n.strings[key] || key;
  if (!params) return tmpl;
  return tmpl.replace(/%\{(\w+)\}/g, (_, name) =>
    params[name] !== undefined ? String(params[name]) : `%{${name}}`,
  );
}

// Apply translations to every element in the DOM that has a `data-i18n` (or
// `data-i18n-placeholder` / `data-i18n-title`) attribute. Called once after
// `/i18n` is fetched, and again whenever translations change.
function applyStaticTranslations() {
  document.querySelectorAll("[data-i18n]").forEach((el) => {
    const key = el.getAttribute("data-i18n");
    if (key) el.textContent = t(key);
  });
  document.querySelectorAll("[data-i18n-placeholder]").forEach((el) => {
    const key = el.getAttribute("data-i18n-placeholder");
    if (key) el.placeholder = t(key);
  });
  document.querySelectorAll("[data-i18n-title]").forEach((el) => {
    const key = el.getAttribute("data-i18n-title");
    if (key) el.title = t(key);
  });
  document.documentElement.lang = i18n.locale;
}

async function loadTranslations() {
  try {
    const resp = await fetch("/i18n", { cache: "no-store" });
    if (!resp.ok) return;
    const data = await resp.json();
    i18n.locale = data.locale || "en";
    i18n.strings = data.strings || {};
    i18n.languages = data.languages || [];
    applyStaticTranslations();
    populateLanguageSelector();
  } catch (_) {
    // Keep the source-language defaults in the HTML.
  }
}

function populateLanguageSelector() {
  const sel = document.getElementById("config-language");
  if (!sel) return;
  sel.innerHTML = "";
  i18n.languages.forEach((lang) => {
    const opt = document.createElement("option");
    opt.value = lang.code;
    opt.textContent = `${lang.flag} ${lang.native_name}`;
    sel.appendChild(opt);
  });
  // Match the active locale to one of the listed options. Compare
  // case-insensitively and fall back to the language prefix so that
  // system locales like "en-US" or "de-DE" resolve to "en" / "de".
  const code = (i18n.locale || "en").toLowerCase();
  let match = i18n.languages.find((l) => l.code.toLowerCase() === code);
  if (!match) {
    const prefix = code.split("-")[0];
    match = i18n.languages.find((l) => l.code.toLowerCase() === prefix);
  }
  if (match) sel.value = match.code;
}

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
  // Mode discovery (works in both config and play servers)
  mode: () => api.json("GET", "/mode"),
  // Play-mode endpoints
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
  // Config-mode endpoints
  configState: () => api.json("GET", "/config"),
  cfgSetAudioDevice: (name) =>
    api.json("POST", "/config/audio-device", { name }),
  cfgSetSampleRate: (rate) =>
    api.json("POST", "/config/sample-rate", { rate }),
  cfgSetIrFile: (path) => api.json("POST", "/config/ir-file", { path }),
  cfgSetOrgan: (path) => api.json("POST", "/config/organ", { path }),
  cfgSetAudioSettings: (body) =>
    api.json("POST", "/config/audio-settings", body),
  cfgUpdateMidiDevice: (body) =>
    api.json("POST", "/config/midi-device", body),
  cfgRescanMidi: () => api.json("POST", "/config/midi/rescan"),
  cfgStart: () => api.json("POST", "/config/start"),
  cfgQuit: () => api.json("POST", "/config/quit"),
  cfgSetLocale: (locale) => api.json("POST", "/config/locale", { locale }),
  cfgBrowse: (path, exts) => {
    const params = new URLSearchParams();
    if (path) params.set("path", path);
    if (exts) params.set("exts", exts);
    const qs = params.toString();
    return api.json("GET", "/config/browse" + (qs ? `?${qs}` : ""));
  },
  cfgAddOrgan: (path, name) =>
    api.json("POST", "/config/library/add-organ", { path, name }),
  cfgRemoveOrgan: (path) =>
    api.json("POST", "/config/library/remove-organ", { path }),
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
  mode: "unknown", // "config" | "play"
  channel: 0,
  stops: [],
  presets: [],
  tremulants: [],
  reverbs: [],
  audio: null,
  config: null, // ConfigStateResponse from /config
};

const wsCtrl = {
  ws: null,
  reconnectTimer: null,
  reconnectDelay: 500,
  openedAt: 0,
  abortController: null,
  modeProbeTimer: null,
};
const WS_DELAY_MAX = 3000;
const WS_DELAY_MIN = 500;
const WS_STABLE_MS = 3000;

// ---------- Mode handling ----------
// Mode discovery deliberately bypasses the shared abort controller. After a
// WS reconnect we *must* learn whether we landed on the config or play
// server before doing anything else; if a stale abort signal cancelled this
// fetch, the page would stay frozen in "unknown" mode and never recover.
async function detectMode() {
  try {
    const resp = await fetch("/mode", { cache: "no-store" });
    if (!resp.ok) return "unknown";
    const m = await resp.json();
    setMode(m.mode);
    return m.mode;
  } catch (_) {
    return "unknown";
  }
}

function setMode(mode) {
  if (state.mode === mode) return;
  state.mode = mode;
  document.body.dataset.mode = mode;
}

// ---------- Tabs (play view) ----------
function setupTabs() {
  const tabs = document.querySelectorAll(".tab[data-tab]");
  const panels = document.querySelectorAll("#play-view .tab-panel");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      const target = tab.dataset.tab;
      tabs.forEach((t) => t.setAttribute("aria-selected", t === tab));
      panels.forEach((p) =>
        p.classList.toggle("active", p.id === `tab-${target}`)
      );
      if (target === "organs") loadOrgans().catch(() => {});
    });
  });
}

// ---------- Tabs (config view) ----------
function setupConfigTabs() {
  const tabs = document.querySelectorAll(".config-tab[data-config-tab]");
  const panels = document.querySelectorAll("#config-view .config-panel");
  tabs.forEach((tab) => {
    tab.addEventListener("click", () => {
      const target = tab.dataset.configTab;
      tabs.forEach((t) => t.setAttribute("aria-selected", t === tab));
      panels.forEach((p) =>
        p.classList.toggle("active", p.id === `config-tab-${target}`)
      );
    });
  });
}

// ---------- Channel selector ----------
function setupChannelSelect() {
  const sel = document.getElementById("stop-channel-select");
  for (let i = 0; i < 16; i++) {
    const opt = document.createElement("option");
    opt.value = String(i);
    opt.textContent = t("channel_fmt", { num: i + 1 });
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

// ---------- Organ info (play mode) ----------
async function refreshOrgan() {
  try {
    const o = await api.organ();
    document.getElementById("organ-name").textContent = o.name || "Rusty Pipes";
  } catch (_) {}
}

// ---------- Stops ----------
async function loadStops() {
  state.stops = await api.stops();
  renderStops();
}

const DIVISION_LABELS = {
  HW: "Hauptwerk",
  SW: "Swell",
  Pos: "Positiv",
  BW: "Brustwerk",
  OW: "Oberwerk",
  So: "Solo",
  P: "Pedal",
  Ped: "Pedal",
  GO: "Grand'Organo",
  PT: "Positivo Tergale",
  Gt: "Great",
  Ch: "Choir",
};

function divisionLabel(id) {
  if (!id) return t("default_division_heading");
  const friendly = DIVISION_LABELS[id];
  return friendly ? `${friendly} (${id})` : id;
}

function stopDisplayName(stop) {
  const div = stop.division;
  const name = stop.name || "";
  if (!div) return name;
  const trimmed = name.trimStart();
  if (trimmed.startsWith(div)) {
    const rest = trimmed.slice(div.length);
    if (rest.length === 0) return name;
    const sep = rest.charCodeAt(0);
    if (sep === 32 || sep === 9 || rest[0] === "." || rest[0] === ":") {
      return rest.replace(/^[\s.:]+/, "");
    }
  }
  return name;
}

function renderStops() {
  const container = document.getElementById("stops-container");
  container.innerHTML = "";
  const groups = new Map();
  state.stops.forEach((s) => {
    const key = s.division || "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(s);
  });

  for (const [division, stops] of groups) {
    const section = document.createElement("section");
    section.className = "division";
    const h = document.createElement("h3");
    h.textContent = divisionLabel(division);
    section.appendChild(h);

    const grid = document.createElement("div");
    grid.className = "stop-grid";

    stops.forEach((stop) => {
      const tile = document.createElement("div");
      tile.className = "stop-tile";
      const isActive = stop.active_channels.includes(state.channel);
      if (isActive) tile.classList.add("active");
      tile.textContent = stopDisplayName(stop);
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
    const set = new Set(stop.active_channels);
    if (active) set.add(state.channel);
    else set.delete(state.channel);
    stop.active_channels = [...set].sort();
    renderStops();
  } catch (e) {
    toast(t("err_stop_toggle_fmt", { err: e.message }), { error: true });
  }
}

function openStopActions(stop) {
  document.getElementById("stop-actions-title").textContent = stop.name;
  document.getElementById("stop-actions-subtitle").textContent = t(
    "stop_actions_channel_fmt",
    { num: state.channel + 1 },
  );
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
      toast(
        t("toast_cleared_stop_fmt", {
          name: stop.name,
          num: state.channel + 1,
        }),
      );
    } catch (e) {
      toast(t("err_clear_fmt", { err: e.message }), { error: true });
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
    name.textContent = preset.name || t("preset_empty");
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
    toast(t("toast_loaded_fmt", { name: preset.name || `F${preset.slot}` }));
    await loadStops();
  } catch (e) {
    toast(t("err_load_fmt", { err: e.message }), { error: true });
  }
}

function openPresetActions(preset) {
  document.getElementById("preset-actions-title").textContent = preset.name
    ? t("modal_preset_title_named_fmt", { num: preset.slot, name: preset.name })
    : t("modal_preset_title_fmt", { num: preset.slot });
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
      toast(t("toast_cleared_preset_fmt", { num: preset.slot }));
    } catch (e) {
      toast(t("err_clear_fmt", { err: e.message }), { error: true });
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
      toast(t("toast_saved_fmt", { name }));
      closeModal("modal-save-preset");
      loadPresets();
    } catch (e) {
      toast(t("err_save_fmt", { err: e.message }), { error: true });
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
    p.textContent = t("no_tremulants");
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
    toast(t("err_tremulant_fmt", { err: e.message }), { error: true });
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
      toast(
        t("toast_cleared_tremulant_fmt", { name: trem.name || trem.id }),
      );
    } catch (e) {
      toast(t("err_clear_fmt", { err: e.message }), { error: true });
    }
  };
  openModal("modal-tremulant-actions");
}

// ---------- Organs library (play mode) ----------
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
    p.textContent = t("organs_none");
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
      badge.textContent = t("organ_badge_current");
      item.appendChild(badge);
    }

    item.addEventListener("click", () => requestLoadOrgan(entry));
    container.appendChild(item);
  });
}

async function requestLoadOrgan(entry) {
  const currentName = document.getElementById("organ-name").textContent;
  if (entry.name === currentName) {
    toast(t("organ_already_loaded_fmt", { name: entry.name }));
    return;
  }
  if (!confirm(t("organ_load_confirm_fmt", { name: entry.name }))) return;
  try {
    await api.loadOrgan(entry.path);
    toast(t("organ_loading_fmt", { name: entry.name }));
  } catch (e) {
    toast(t("err_load_fmt", { err: e.message }), { error: true });
  }
}

// ---------- Audio settings (play mode) ----------
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
  noneOpt.textContent = t("reverb_disabled");
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
      toast(t("err_gain_fmt", { err: e.message }), { error: true }),
    );
  });

  const poly = document.getElementById("polyphony-slider");
  const polyVal = document.getElementById("polyphony-value");
  poly.addEventListener("input", () => {
    polyVal.textContent = String(poly.value);
  });
  poly.addEventListener("change", () => {
    api.setPolyphony(Number(poly.value)).catch((e) =>
      toast(t("err_polyphony_fmt", { err: e.message }), { error: true }),
    );
  });

  const reverbSel = document.getElementById("reverb-select");
  reverbSel.addEventListener("change", () => {
    api.selectReverb(Number(reverbSel.value)).catch((e) =>
      toast(t("err_reverb_fmt", { err: e.message }), { error: true }),
    );
  });

  const mix = document.getElementById("reverb-mix-slider");
  const mixVal = document.getElementById("reverb-mix-value");
  mix.addEventListener("input", () => {
    mixVal.textContent = Number(mix.value).toFixed(2);
  });
  mix.addEventListener("change", () => {
    api.setReverbMix(Number(mix.value)).catch((e) =>
      toast(t("err_mix_fmt", { err: e.message }), { error: true }),
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
    ? t("rec_midi_stop")
    : t("rec_midi_start");
  audioBtn.classList.toggle("on", a.is_recording_audio);
  audioBtn.textContent = a.is_recording_audio
    ? t("rec_audio_stop")
    : t("rec_audio_start");
}

function setupRecordingControls() {
  document
    .getElementById("record-midi-btn")
    .addEventListener("click", async () => {
      const newState = !state.audio?.is_recording_midi;
      try {
        await api.recordMidi(newState);
        state.audio.is_recording_midi = newState;
        renderRecording();
        toast(
          newState
            ? t("toast_rec_midi_started")
            : t("toast_rec_midi_saved"),
        );
      } catch (e) {
        toast(t("err_recording_fmt", { err: e.message }), { error: true });
      }
    });
  document
    .getElementById("record-audio-btn")
    .addEventListener("click", async () => {
      const newState = !state.audio?.is_recording_audio;
      try {
        await api.recordAudio(newState);
        state.audio.is_recording_audio = newState;
        renderRecording();
        toast(
          newState
            ? t("toast_rec_audio_started")
            : t("toast_rec_audio_saved"),
        );
      } catch (e) {
        toast(t("err_recording_fmt", { err: e.message }), { error: true });
      }
    });
}

// ---------- Panic ----------
document.getElementById("panic-btn").addEventListener("click", async () => {
  try {
    await api.panic();
    toast(t("toast_panic"));
  } catch (e) {
    toast(t("err_panic_fmt", { err: e.message }), { error: true });
  }
});

// ---------- MIDI Learn ----------
let learnAutoCloseHandle = null;
let learnActive = false;

async function startLearn(targetBody) {
  try {
    const resp = await api.midiLearnStart(targetBody);
    document.getElementById("learn-target-label").textContent =
      resp.target_name || "(target)";
    document.getElementById("learn-state-label").textContent = t("learn_waiting");
    document.getElementById("learn-result").textContent = "";
    if (learnAutoCloseHandle) {
      clearTimeout(learnAutoCloseHandle);
      learnAutoCloseHandle = null;
    }
    learnActive = true;
    openModal("modal-learn");
  } catch (e) {
    toast(t("err_learn_fmt", { err: e.message }), { error: true });
  }
}

function handleLearnUpdate(msg) {
  if (!learnActive) return;
  if (msg.state === "captured") {
    document.getElementById("learn-state-label").textContent = t("learn_done");
    document.getElementById("learn-result").textContent =
      msg.event_description || "";
    toast(t("toast_learned_fmt", { event: msg.event_description || "event" }));
    learnActive = false;
    learnAutoCloseHandle = setTimeout(() => closeModal("modal-learn"), 1100);
  } else if (msg.state === "timed_out") {
    document.getElementById("learn-state-label").textContent = t("learn_timed_out");
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

// =============================================================================
// CONFIG VIEW
// =============================================================================

async function loadConfigState() {
  state.config = await api.configState();
  renderConfig();
}

function renderConfig() {
  if (!state.config) return;
  const c = state.config;

  // --- Topbar title: always "Configuration" while in config mode ---
  document.getElementById("organ-name").textContent = t("topbar_config");

  // --- Organ list ---
  renderConfigOrgans();

  // --- Audio device combo ---
  const adev = document.getElementById("config-audio-device");
  adev.innerHTML = "";
  const defaultOpt = document.createElement("option");
  defaultOpt.value = "";
  defaultOpt.textContent = t("config_audio_device_default");
  adev.appendChild(defaultOpt);
  c.available_audio_devices.forEach((name) => {
    const opt = document.createElement("option");
    opt.value = name;
    opt.textContent = name;
    adev.appendChild(opt);
  });
  adev.value = c.selected_audio_device_name || "";

  // --- Sample rate combo ---
  const sr = document.getElementById("config-sample-rate");
  sr.innerHTML = "";
  c.available_sample_rates.forEach((rate) => {
    const opt = document.createElement("option");
    opt.value = String(rate);
    opt.textContent = `${rate} Hz`;
    sr.appendChild(opt);
  });
  sr.value = String(c.settings.sample_rate);

  // --- IR file combo ---
  const ir = document.getElementById("config-ir-file");
  ir.innerHTML = "";
  const noneOpt = document.createElement("option");
  noneOpt.value = "";
  noneOpt.textContent = t("config_reverb_none");
  ir.appendChild(noneOpt);
  c.available_ir_files.forEach((entry) => {
    const opt = document.createElement("option");
    opt.value = entry.path;
    opt.textContent = entry.name;
    ir.appendChild(opt);
  });
  ir.value = c.settings.ir_file || "";

  // --- Sliders / toggles ---
  setSliderValue("config-gain", "config-gain-value", c.settings.gain, 2);
  const polySlider = document.getElementById("config-polyphony");
  if (c.settings.polyphony > Number(polySlider.max)) {
    polySlider.max = String(c.settings.polyphony);
  }
  polySlider.value = c.settings.polyphony;
  document.getElementById("config-polyphony-value").textContent = String(
    c.settings.polyphony,
  );
  setSliderValue(
    "config-reverb-mix",
    "config-reverb-mix-value",
    c.settings.reverb_mix,
    2,
  );

  document.getElementById("config-buffer").value =
    c.settings.audio_buffer_frames;
  setSliderValue(
    "config-max-ram",
    "config-max-ram-value",
    c.settings.max_ram_gb,
    1,
  );
  document.getElementById("config-max-ram").disabled = c.settings.precache;

  document.getElementById("config-precache").checked = c.settings.precache;
  document.getElementById("config-convert-16bit").checked =
    c.settings.convert_to_16bit;
  document.getElementById("config-original-tuning").checked =
    c.settings.original_tuning;

  // --- MIDI device list ---
  renderConfigMidiList();

  // --- Start button ---
  const startBtn = document.getElementById("config-start-btn");
  const warning = document.getElementById("config-start-warning");
  const hasOrgan = !!c.settings.organ_file;
  startBtn.disabled = !hasOrgan;
  warning.style.display = hasOrgan ? "none" : "";
}

function setSliderValue(sliderId, valueId, value, decimals) {
  const slider = document.getElementById(sliderId);
  const label = document.getElementById(valueId);
  slider.value = value;
  if (label) label.textContent = Number(value).toFixed(decimals);
}

// ---------- File browser ----------
// Single shared instance — only one browser modal can be open at a time.
const fileBrowser = {
  exts: null, // comma-separated extensions or null for all files
  parentPath: null,
  // Resolve callback set by openFileBrowser; called with the chosen path
  // (or null if the user cancels by closing the modal).
  resolve: null,
};

async function openFileBrowser({ title, exts, initialPath }) {
  fileBrowser.exts = exts || null;
  document.getElementById("file-browser-title").textContent =
    title || t("file_browser_title");
  await navigateFileBrowser(initialPath || null);
  openModal("modal-file-browser");
  return new Promise((resolve) => {
    fileBrowser.resolve = resolve;
  });
}

async function navigateFileBrowser(path) {
  try {
    const data = await api.cfgBrowse(path, fileBrowser.exts);
    fileBrowser.parentPath = data.parent_path;
    document.getElementById("file-browser-current-path").value =
      data.current_path;
    renderFileBrowserEntries(data.entries);
  } catch (e) {
    toast(t("err_browse_fmt", { err: e.message }), { error: true });
  }
}

function renderFileBrowserEntries(entries) {
  const container = document.getElementById("file-browser-entries");
  container.innerHTML = "";
  const empty = document.getElementById("file-browser-empty");
  if (!entries || entries.length === 0) {
    empty.classList.remove("hidden");
    return;
  }
  empty.classList.add("hidden");
  entries.forEach((entry) => {
    const row = document.createElement("div");
    row.className = "file-entry " + (entry.is_dir ? "dir" : "file");
    const icon = document.createElement("span");
    icon.className = "icon";
    icon.textContent = entry.is_dir ? "📁" : "📄";
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = entry.name;
    row.appendChild(icon);
    row.appendChild(name);
    if (!entry.is_dir && entry.size != null) {
      const size = document.createElement("span");
      size.className = "size";
      size.textContent = formatFileSize(entry.size);
      row.appendChild(size);
    }
    row.addEventListener("click", () => {
      if (entry.is_dir) {
        navigateFileBrowser(entry.path);
      } else {
        const resolve = fileBrowser.resolve;
        fileBrowser.resolve = null;
        closeModal("modal-file-browser");
        if (resolve) resolve(entry.path);
      }
    });
    container.appendChild(row);
  });
}

function formatFileSize(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Wire the browser's static controls once at startup.
function setupFileBrowser() {
  document.getElementById("file-browser-up").addEventListener("click", () => {
    if (fileBrowser.parentPath) navigateFileBrowser(fileBrowser.parentPath);
  });
  // Cancel resolves with null so callers can distinguish cancellation from
  // a chosen file.
  document
    .querySelectorAll("#modal-file-browser [data-modal-close]")
    .forEach((btn) => {
      btn.addEventListener("click", () => {
        const resolve = fileBrowser.resolve;
        fileBrowser.resolve = null;
        if (resolve) resolve(null);
      });
    });
}

function renderConfigOrgans() {
  const c = state.config;
  const container = document.getElementById("config-organ-list");
  container.innerHTML = "";
  if (!c.organ_library || c.organ_library.length === 0) {
    const p = document.createElement("p");
    p.className = "muted";
    p.textContent = t("config_organs_empty");
    container.appendChild(p);
    return;
  }
  c.organ_library.forEach((entry) => {
    const item = document.createElement("div");
    item.className = "organ-item";
    const isCurrent = entry.path === c.settings.organ_file;
    if (isCurrent) item.classList.add("current");

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

    if (isCurrent) {
      const badge = document.createElement("span");
      badge.className = "badge";
      badge.textContent = t("config_organ_badge_selected");
      item.appendChild(badge);
    }

    // Selecting and removing share the same row, so the remove button
    // stops propagation to avoid also firing the row's selection click.
    const removeBtn = document.createElement("button");
    removeBtn.className = "ghost small organ-remove";
    removeBtn.textContent = "×";
    removeBtn.title = t("config_btn_remove_organ");
    removeBtn.addEventListener("click", async (ev) => {
      ev.stopPropagation();
      if (!confirm(t("config_remove_organ_confirm_fmt", { name: entry.name })))
        return;
      try {
        await api.cfgRemoveOrgan(entry.path);
        toast(t("config_removed_organ_fmt", { name: entry.name }));
      } catch (e) {
        toast(t("err_update_fmt", { err: e.message }), { error: true });
      }
    });
    item.appendChild(removeBtn);

    item.addEventListener("click", async () => {
      try {
        await api.cfgSetOrgan(entry.path);
        // Optimistic local update so the "Selected" badge moves
        // immediately, without waiting for the WS Refetch round-trip.
        if (state.config) {
          state.config.settings.organ_file = entry.path;
          renderConfig();
        }
        toast(t("config_selected_fmt", { name: entry.name }));
      } catch (e) {
        toast(t("err_selection_fmt", { err: e.message }), { error: true });
      }
    });
    container.appendChild(item);
  });
}

function renderConfigMidiList() {
  const c = state.config;
  const container = document.getElementById("config-midi-list");
  container.innerHTML = "";
  if (!c.system_midi_ports || c.system_midi_ports.length === 0) {
    const p = document.createElement("p");
    p.className = "muted";
    p.textContent = t("config_midi_none");
    container.appendChild(p);
    return;
  }
  c.system_midi_ports.forEach((port) => {
    const cfg = c.settings.midi_devices.find((d) => d.name === port.name) || {
      name: port.name,
      enabled: false,
      mapping_mode: "Simple",
      simple_target_channel: 0,
      complex_mapping: Array.from({ length: 16 }, (_, i) => i),
    };

    const wrap = document.createElement("div");
    wrap.className = "config-midi-device";
    if (!cfg.enabled) wrap.classList.add("disabled");

    const header = document.createElement("div");
    header.className = "header";

    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = !!cfg.enabled;
    cb.addEventListener("change", async () => {
      try {
        await api.cfgUpdateMidiDevice({
          name: port.name,
          enabled: cb.checked,
        });
      } catch (e) {
        toast(t("err_update_fmt", { err: e.message }), { error: true });
      }
    });
    header.appendChild(cb);

    const nameEl = document.createElement("span");
    nameEl.className = "name";
    nameEl.textContent = port.name;
    header.appendChild(nameEl);

    const mapBtn = document.createElement("button");
    mapBtn.className = "ghost small";
    mapBtn.textContent = t("config_midi_map_button");
    mapBtn.addEventListener("click", () => openMidiMappingModal(port.name));
    header.appendChild(mapBtn);

    wrap.appendChild(header);

    const summary = document.createElement("div");
    summary.className = "summary";
    summary.textContent = midiMappingSummary(cfg);
    wrap.appendChild(summary);

    container.appendChild(wrap);
  });
}

function midiMappingSummary(cfg) {
  if (cfg.mapping_mode === "Simple") {
    return t("config_midi_summary_simple_fmt", {
      num: cfg.simple_target_channel + 1,
    });
  }
  const notes = [];
  cfg.complex_mapping.forEach((target, i) => {
    if (target !== i) notes.push(`${i + 1}→${target + 1}`);
  });
  if (notes.length === 0) return t("config_midi_summary_complex_default");
  return t("config_midi_summary_complex_fmt", { notes: notes.join(", ") });
}

function openMidiMappingModal(deviceName) {
  const c = state.config;
  const cfg = c.settings.midi_devices.find((d) => d.name === deviceName);
  if (!cfg) return;
  document.getElementById("midi-mapping-title").textContent = t(
    "midi_modal_title_fmt",
    { name: deviceName },
  );

  const radios = document.querySelectorAll(
    "input[name='midi-mapping-mode']",
  );
  radios.forEach((r) => {
    r.checked = r.value === cfg.mapping_mode;
    r.onchange = async () => {
      if (r.checked) {
        try {
          await api.cfgUpdateMidiDevice({
            name: deviceName,
            mapping_mode: r.value,
          });
          await loadConfigState();
          openMidiMappingModal(deviceName);
        } catch (e) {
          toast(t("err_update_fmt", { err: e.message }), { error: true });
        }
      }
    };
  });

  const simpleSection = document.getElementById("midi-mapping-simple");
  const complexSection = document.getElementById("midi-mapping-complex");
  if (cfg.mapping_mode === "Simple") {
    simpleSection.classList.remove("hidden");
    complexSection.classList.add("hidden");
    const sel = document.getElementById("midi-mapping-simple-channel");
    sel.innerHTML = "";
    for (let i = 0; i < 16; i++) {
      const opt = document.createElement("option");
      opt.value = String(i);
      opt.textContent = t("channel_fmt", { num: i + 1 });
      sel.appendChild(opt);
    }
    sel.value = String(cfg.simple_target_channel);
    sel.onchange = async () => {
      try {
        await api.cfgUpdateMidiDevice({
          name: deviceName,
          simple_target_channel: Number(sel.value),
        });
      } catch (e) {
        toast(t("err_update_fmt", { err: e.message }), { error: true });
      }
    };
  } else {
    simpleSection.classList.add("hidden");
    complexSection.classList.remove("hidden");
    const grid = document.getElementById("midi-mapping-grid");
    grid.innerHTML = "";
    cfg.complex_mapping.forEach((target, i) => {
      const row = document.createElement("div");
      row.className = "row";
      const label = document.createElement("label");
      label.textContent = t("midi_modal_input_fmt", { num: i + 1 });
      const sel = document.createElement("select");
      for (let target2 = 0; target2 < 16; target2++) {
        const opt = document.createElement("option");
        opt.value = String(target2);
        opt.textContent = String(target2 + 1);
        sel.appendChild(opt);
      }
      sel.value = String(target);
      sel.addEventListener("change", async () => {
        const newMap = cfg.complex_mapping.slice();
        newMap[i] = Number(sel.value);
        try {
          await api.cfgUpdateMidiDevice({
            name: deviceName,
            complex_mapping: newMap,
          });
        } catch (e) {
          toast(t("err_update_fmt", { err: e.message }), { error: true });
        }
      });
      row.appendChild(label);
      row.appendChild(sel);
      grid.appendChild(row);
    });
  }

  openModal("modal-midi-mapping");
}

function setupConfigControls() {
  // --- Audio device ---
  document
    .getElementById("config-audio-device")
    .addEventListener("change", async (e) => {
      const name = e.target.value || null;
      try {
        await api.cfgSetAudioDevice(name);
      } catch (err) {
        toast(t("err_audio_device_fmt", { err: err.message }), { error: true });
      }
    });

  // --- Sample rate ---
  document
    .getElementById("config-sample-rate")
    .addEventListener("change", async (e) => {
      try {
        await api.cfgSetSampleRate(Number(e.target.value));
      } catch (err) {
        toast(t("err_sample_rate_fmt", { err: err.message }), { error: true });
      }
    });

  // --- IR file ---
  document
    .getElementById("config-ir-file")
    .addEventListener("change", async (e) => {
      try {
        await api.cfgSetIrFile(e.target.value || null);
      } catch (err) {
        toast(t("err_ir_file_fmt", { err: err.message }), { error: true });
      }
    });

  // --- Reverb mix slider ---
  const mix = document.getElementById("config-reverb-mix");
  const mixVal = document.getElementById("config-reverb-mix-value");
  mix.addEventListener("input", () => {
    mixVal.textContent = Number(mix.value).toFixed(2);
  });
  mix.addEventListener("change", async () => {
    try {
      await api.cfgSetAudioSettings({ reverb_mix: Number(mix.value) });
    } catch (e) {
      toast(t("err_mix_fmt", { err: e.message }), { error: true });
    }
  });

  // --- Gain slider ---
  const gain = document.getElementById("config-gain");
  const gainVal = document.getElementById("config-gain-value");
  gain.addEventListener("input", () => {
    gainVal.textContent = Number(gain.value).toFixed(2);
  });
  gain.addEventListener("change", async () => {
    try {
      await api.cfgSetAudioSettings({ gain: Number(gain.value) });
    } catch (e) {
      toast(t("err_gain_fmt", { err: e.message }), { error: true });
    }
  });

  // --- Polyphony slider ---
  const poly = document.getElementById("config-polyphony");
  const polyVal = document.getElementById("config-polyphony-value");
  poly.addEventListener("input", () => {
    polyVal.textContent = String(poly.value);
  });
  poly.addEventListener("change", async () => {
    try {
      await api.cfgSetAudioSettings({ polyphony: Number(poly.value) });
    } catch (e) {
      toast(t("err_polyphony_fmt", { err: e.message }), { error: true });
    }
  });

  // --- Buffer ---
  document.getElementById("config-buffer").addEventListener("change", async (e) => {
    try {
      await api.cfgSetAudioSettings({
        audio_buffer_frames: Number(e.target.value),
      });
    } catch (err) {
      toast(t("err_buffer_fmt", { err: err.message }), { error: true });
    }
  });

  // --- Max RAM slider ---
  const ram = document.getElementById("config-max-ram");
  const ramVal = document.getElementById("config-max-ram-value");
  ram.addEventListener("input", () => {
    ramVal.textContent = Number(ram.value).toFixed(1);
  });
  ram.addEventListener("change", async () => {
    try {
      await api.cfgSetAudioSettings({ max_ram_gb: Number(ram.value) });
    } catch (e) {
      toast(t("err_ram_fmt", { err: e.message }), { error: true });
    }
  });

  // --- Boolean options ---
  const boolMap = {
    "config-precache": "precache",
    "config-convert-16bit": "convert_to_16bit",
    "config-original-tuning": "original_tuning",
  };
  Object.entries(boolMap).forEach(([elId, field]) => {
    document.getElementById(elId).addEventListener("change", async (e) => {
      try {
        await api.cfgSetAudioSettings({ [field]: e.target.checked });
      } catch (err) {
        toast(t("err_update_fmt", { err: err.message }), { error: true });
      }
    });
  });

  // --- Rescan MIDI ---
  document
    .getElementById("config-rescan-midi")
    .addEventListener("click", async () => {
      try {
        await api.cfgRescanMidi();
        toast(t("config_midi_rescan_done"));
      } catch (e) {
        toast(t("err_rescan_fmt", { err: e.message }), { error: true });
      }
    });

  // --- Start ---
  document
    .getElementById("config-start-btn")
    .addEventListener("click", async () => {
      // Toast first, before issuing the request. The WS will close shortly
      // (when the config server is dropped); if our fetch happens to be
      // aborted by the resulting reconnect logic, AbortError is expected
      // and not user-visible.
      toast(t("toast_loading_organ"));
      try {
        await api.cfgStart();
      } catch (e) {
        if (e.name === "AbortError") return;
        toast(t("err_start_fmt", { err: e.message }), { error: true });
      }
    });

  // --- Quit ---
  document
    .getElementById("config-quit-btn")
    .addEventListener("click", async () => {
      if (!confirm(t("config_quit_confirm"))) return;
      try {
        await api.cfgQuit();
      } catch (e) {
        if (e.name === "AbortError") return;
        toast(t("err_quit_fmt", { err: e.message }), { error: true });
      }
    });

  // --- Add organ button ---
  document
    .getElementById("config-add-organ-btn")
    .addEventListener("click", async () => {
      const picked = await openFileBrowser({
        title: t("file_browser_title_organ"),
        exts: "organ,orgue,Organ_Hauptwerk_xml,xml",
      });
      if (!picked) return;
      try {
        await api.cfgAddOrgan(picked, null);
        toast(t("config_added_organ_fmt", { path: picked }));
      } catch (e) {
        toast(t("err_update_fmt", { err: e.message }), { error: true });
      }
    });

  // --- Browse for IR file ---
  document
    .getElementById("config-browse-ir-btn")
    .addEventListener("click", async () => {
      const picked = await openFileBrowser({
        title: t("file_browser_title_ir"),
        exts: "wav,flac,mp3",
      });
      if (!picked) return;
      try {
        await api.cfgSetIrFile(picked);
        toast(t("config_ir_selected_fmt", { path: picked }));
      } catch (e) {
        toast(t("err_ir_file_fmt", { err: e.message }), { error: true });
      }
    });

  // --- Language selector ---
  document
    .getElementById("config-language")
    .addEventListener("change", async (e) => {
      const code = e.target.value;
      try {
        await api.cfgSetLocale(code);
        // Re-fetch translations and re-render dynamic text. Static
        // `data-i18n` text is updated by applyStaticTranslations() which
        // loadTranslations() calls.
        await loadTranslations();
        if (state.config) renderConfig();
      } catch (err) {
        toast(t("err_update_fmt", { err: err.message }), { error: true });
      }
    });
}

// =============================================================================
// WebSocket
// =============================================================================
function setStatus(state, label) {
  const el = document.getElementById("status-dot");
  if (!el) return;
  el.classList.remove("connected", "connecting", "reconnecting");
  el.classList.add(state);
  el.title = label;
}

function scheduleReconnect() {
  if (wsCtrl.reconnectTimer) return;
  const secs = Math.ceil(wsCtrl.reconnectDelay / 1000);
  setStatus("reconnecting", t("status_reconnecting_fmt", { secs }));
  wsCtrl.reconnectTimer = setTimeout(() => {
    wsCtrl.reconnectTimer = null;
    connectWebSocket();
  }, wsCtrl.reconnectDelay);
  wsCtrl.reconnectDelay = Math.min(wsCtrl.reconnectDelay * 2, WS_DELAY_MAX);
  startModeProbe();
}

function reconnectNow() {
  if (wsCtrl.reconnectTimer) {
    clearTimeout(wsCtrl.reconnectTimer);
    wsCtrl.reconnectTimer = null;
  }
  if (wsCtrl.ws && wsCtrl.ws.readyState === WebSocket.OPEN) return;
  wsCtrl.reconnectDelay = WS_DELAY_MIN;
  connectWebSocket();
}

// While disconnected, periodically probe /mode. When the play server
// finishes loading the organ and starts up on the same port, /mode begins
// responding — at which point we trigger an immediate WS reconnect (skipping
// the exponential-backoff wait) and update the page mode. This is the
// fallback path that recovers the UI even if the WS reconnect stalls.
const MODE_PROBE_INTERVAL_MS = 1500;
function startModeProbe() {
  if (wsCtrl.modeProbeTimer) return;
  wsCtrl.modeProbeTimer = setInterval(async () => {
    if (wsCtrl.ws && wsCtrl.ws.readyState === WebSocket.OPEN) {
      stopModeProbe();
      return;
    }
    try {
      const resp = await fetch("/mode", { cache: "no-store" });
      if (!resp.ok) return;
      const m = await resp.json();
      const newMode = m.mode;
      if (state.mode !== newMode) {
        setMode(newMode);
        // The HTTP server is up; force the WS to attempt reconnection now
        // and load the right view.
        if (newMode === "play") refetchAllPlay();
        else if (newMode === "config") refetchAllConfig();
      }
      // Server is reachable — try WS reconnect immediately.
      reconnectNow();
    } catch (_) {
      // Server still down; keep probing.
    }
  }, MODE_PROBE_INTERVAL_MS);
}
function stopModeProbe() {
  if (wsCtrl.modeProbeTimer) {
    clearInterval(wsCtrl.modeProbeTimer);
    wsCtrl.modeProbeTimer = null;
  }
}

function connectWebSocket() {
  if (wsCtrl.ws) {
    try {
      wsCtrl.ws.onopen =
        wsCtrl.ws.onmessage =
        wsCtrl.ws.onerror =
        wsCtrl.ws.onclose =
          null;
      wsCtrl.ws.close();
    } catch (_) {}
    wsCtrl.ws = null;
  }

  let ws;
  try {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    ws = new WebSocket(`${proto}//${location.host}/ws`);
  } catch (_) {
    scheduleReconnect();
    return;
  }
  wsCtrl.ws = ws;
  setStatus("connecting", t("status_connecting"));

  ws.addEventListener("open", () => {
    if (ws !== wsCtrl.ws) return;
    wsCtrl.openedAt = Date.now();
    wsCtrl.reconnectDelay = WS_DELAY_MIN;
    wsCtrl.abortController = new AbortController();
    stopModeProbe();
    setStatus("connected", t("status_connected"));
    // Re-fetch translations on every reconnect: the active locale and
    // string set are the same across config/play servers in this build,
    // but a future build that allows changing the locale at runtime would
    // benefit from the refresh.
    loadTranslations().catch(() => {});
    // The mode might have changed since we last connected (config →
    // play after Start). Re-detect and reload the appropriate state.
    detectMode().then((m) => {
      if (m === "play") {
        refetchAllPlay();
      } else if (m === "config") {
        refetchAllConfig();
      }
    });
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
    if (wsCtrl.abortController) {
      try {
        wsCtrl.abortController.abort();
      } catch (_) {}
      wsCtrl.abortController = null;
    }
    if (wsCtrl.openedAt && Date.now() - wsCtrl.openedAt > WS_STABLE_MS) {
      wsCtrl.reconnectDelay = WS_DELAY_MIN;
    }
    wsCtrl.openedAt = 0;
    scheduleReconnect();
  });

  ws.addEventListener("error", () => {});
}

window.addEventListener("online", reconnectNow);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") reconnectNow();
});

function handleWsMessage(msg) {
  switch (msg.type) {
    case "Refetch":
      // Reload translations too — covers the case where another client
      // changed the active locale and we need to pick up the new strings.
      loadTranslations()
        .catch(() => {})
        .finally(() => {
          if (state.mode === "play") {
            refetchAllPlay();
          } else if (state.mode === "config") {
            refetchAllConfig();
          }
        });
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
      // The server is shutting down. Could be:
      // (1) Config server → play server (we clicked Start)
      // (2) Play server → play server (organ reload)
      // (3) Config server → exit (we clicked Quit)
      // In all cases: abort fetches, set "unknown" mode until reconnect.
      toast(t("toast_reloading"));
      setMode("unknown");
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

function refetchAllPlay() {
  Promise.allSettled([
    refreshOrgan().then(() => loadOrgans().catch(() => {})),
    loadStops(),
    loadPresets(),
    loadTremulants(),
    loadAudio(),
  ]);
}

function refetchAllConfig() {
  loadConfigState().catch(() => {});
}

// =============================================================================
// Init
// =============================================================================
async function init() {
  // Load translations first so subsequent setup uses the localized strings.
  await loadTranslations();

  setupTabs();
  setupConfigTabs();
  setupChannelSelect();
  setupAudioControls();
  setupRecordingControls();
  setupConfigControls();
  setupFileBrowser();

  // Mark initial config-tab as active for visibility logic
  document
    .querySelector(".config-tab[data-config-tab='organ']")
    ?.setAttribute("aria-selected", "true");

  const mode = await detectMode();
  if (mode === "play") {
    await refreshOrgan();
    await Promise.allSettled([
      loadStops(),
      loadPresets(),
      loadTremulants(),
      loadAudio(),
    ]);
  } else if (mode === "config") {
    await loadConfigState().catch(() => {});
  }
  connectWebSocket();
}

init();
