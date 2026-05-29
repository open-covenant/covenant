"use client";

import { useEffect, useRef } from "react";

const SITE_KEY = process.env.NEXT_PUBLIC_TURNSTILE_SITE_KEY;
const SCRIPT = "https://challenges.cloudflare.com/turnstile/v0/api.js";

declare global {
  interface Window {
    turnstile?: {
      render: (
        el: HTMLElement,
        opts: {
          sitekey: string;
          callback: (token: string) => void;
          "expired-callback"?: () => void;
          "error-callback"?: () => void;
          appearance?: "always" | "execute" | "interaction-only";
        },
      ) => string;
      reset: (id?: string) => void;
    };
    __covTurnstileReset?: () => void;
  }
}

/** True when Turnstile is configured (site key present). */
export const turnstileEnabled = Boolean(SITE_KEY);

/**
 * Cloudflare Turnstile widget. Renders only when NEXT_PUBLIC_TURNSTILE_SITE_KEY
 * is set, so the sandbox works unchanged until Turnstile is configured. Reports
 * the token (or null on expiry/error) via onToken; exposes a global reset so
 * the form can clear it after a submit.
 */
export function Turnstile({ onToken }: { onToken: (token: string | null) => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const widgetId = useRef<string | null>(null);

  useEffect(() => {
    if (!SITE_KEY) return;
    let interval: ReturnType<typeof setInterval> | undefined;

    const render = () => {
      if (!window.turnstile || !ref.current || widgetId.current) return;
      widgetId.current = window.turnstile.render(ref.current, {
        sitekey: SITE_KEY,
        callback: (token) => onToken(token),
        "expired-callback": () => onToken(null),
        "error-callback": () => onToken(null),
        // Hide the widget unless Cloudflare's risk check actually decides a
        // human interaction is needed. The background pass still runs, so the
        // submit-disabled gate still resolves on the token callback within a
        // few hundred ms for legitimate visitors. Requires the site key to be
        // configured for "Managed" (or "Non-Interactive") in Cloudflare —
        // a key set to "Always Interactive" overrides this and stays visible.
        appearance: "interaction-only",
      });
      window.__covTurnstileReset = () => {
        if (widgetId.current) window.turnstile?.reset(widgetId.current);
        onToken(null);
      };
    };

    if (window.turnstile) {
      render();
    } else {
      if (!document.querySelector(`script[src="${SCRIPT}"]`)) {
        const s = document.createElement("script");
        s.src = SCRIPT;
        s.async = true;
        document.head.appendChild(s);
      }
      interval = setInterval(() => {
        if (window.turnstile) {
          clearInterval(interval);
          render();
        }
      }, 200);
    }
    return () => {
      if (interval) clearInterval(interval);
    };
  }, [onToken]);

  if (!SITE_KEY) return null;
  return <div ref={ref} style={{ marginTop: 10 }} />;
}
