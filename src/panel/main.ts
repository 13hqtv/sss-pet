import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";
import App from "./App.svelte";
import { applyTheme } from "./lib/theme";
import "./app.css";

// Apply the theme BEFORE mounting Svelte so the first paint is themed.
// The CSS @media fallback covers the pre-JS moment; this makes the
// user's persisted choice authoritative. settings_get is the same
// command SettingsView uses, so the value stays in sync.
invoke<{ theme?: string }>("settings_get")
  .then((s) => {
    applyTheme(s?.theme);
  })
  .catch((e) => {
    // settings_get failed (e.g. first run, config not yet written) →
    // fall back to system theme. The CSS @media query keeps the panel
    // correct until SettingsView sets data-theme on save.
    console.warn("theme: settings_get failed, falling back to system", e);
    applyTheme("system");
  });

const target = document.getElementById("app");
if (!target) throw new Error("#app element not found");

const app = mount(App, { target });

export default app;
