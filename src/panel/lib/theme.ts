// Task 1: panel theme application. A single shared matchMedia listener
// for the "system" path so main.ts (startup) and SettingsView (save)
// don't double-register or fight over <html data-theme>.
//
// The pet window (pet.html) does NOT use this — per the design spec,
// the robot keeps a fixed color scheme regardless of theme, so only the
// panel webview themes.

let systemListener: ((e: MediaQueryListEvent) => void) | null = null;
let systemMql: MediaQueryList | null = null;

/** Resolve the OS prefers-color-scheme to a concrete theme value. */
function resolveSystem(): "light" | "dark" {
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

/** Tear down the current system listener (if any). Called before
 * installing a new one or when switching to an explicit theme. */
function detachSystemListener(): void {
  if (systemListener && systemMql) {
    systemMql.removeEventListener("change", systemListener);
  }
  systemListener = null;
  systemMql = null;
}

/** Apply a theme choice ("system" | "light" | "dark") to
 * <html data-theme>. For "system" (and missing/unknown), resolves via
 * prefers-color-scheme and registers a listener that keeps data-theme
 * in sync with OS changes. For explicit light/dark, the choice is
 * sticky and any prior system listener is removed. Safe to call
 * repeatedly (startup + each settings save) — each call replaces the
 * prior listener registration. */
export function applyTheme(choice: string | null | undefined): void {
  const root = document.documentElement;
  // Always tear down the prior system listener: an explicit choice is
  // sticky (no OS tracking), and a new "system" registration needs a
  // fresh listener. This is what makes repeat calls safe.
  detachSystemListener();

  if (choice === "light" || choice === "dark") {
    root.dataset.theme = choice;
    return;
  }

  // system / missing / unknown → follow OS now + track changes.
  root.dataset.theme = resolveSystem();
  systemMql = window.matchMedia("(prefers-color-scheme: dark)");
  systemListener = () => {
    // Only re-resolve while we're still in system mode. If a later
    // applyTheme switched to explicit light/dark it already detached
    // this listener, so this callback won't fire.
    root.dataset.theme = resolveSystem();
  };
  systemMql.addEventListener("change", systemListener);
}
