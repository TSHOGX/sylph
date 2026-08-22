"use client";

/**
 * Registers the service worker.
 *
 * See `public/sw.js` for what it does — briefly, it makes the app installable on
 * Android and gives a failed navigation an honest offline page. It caches nothing
 * versioned, so there is no stale-asset risk in registering it eagerly.
 *
 * # Production only
 *
 * In development the dev server rebuilds chunks continuously and a worker in the
 * mix is one more thing between an edit and the page. More concretely: workers are
 * scoped per *origin*, and `localhost` is shared with every other project on the
 * machine, so a worker registered here at port 3000 outlives this app and is served
 * to whatever occupies that port next.
 *
 * # Deliberately after `load`
 *
 * Registration competes for bandwidth with the page's own chunks, and the worker is
 * useless on the load that registers it — it activates for the *next* navigation.
 * Waiting until `load` keeps it off the critical path entirely.
 */

import { useEffect } from "react";

export function RegisterServiceWorker() {
  useEffect(() => {
    if (process.env.NODE_ENV !== "production") return;
    if (!("serviceWorker" in navigator)) return;

    const register = () => {
      // Failure is not worth surfacing: everything the worker provides is an
      // enhancement, and the app is fully functional without it. A rejected
      // registration is normal in a private window and on an insecure origin.
      void navigator.serviceWorker.register("/sw.js").catch(() => {});
    };

    // `load` may already have fired by the time React hydrates and runs this.
    if (document.readyState === "complete") {
      register();
      return;
    }
    window.addEventListener("load", register, { once: true });
    return () => window.removeEventListener("load", register);
  }, []);

  return null;
}
