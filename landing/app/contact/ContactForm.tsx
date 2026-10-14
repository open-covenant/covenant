"use client";

import { useState } from "react";

type Status = "idle" | "sending" | "sent" | "error";

const field =
  "w-full border border-line bg-raised/50 px-3 py-3 text-[13px] text-fg placeholder:text-faint outline-none backdrop-blur-sm transition-colors focus:border-muted-2 focus:bg-raised/70 sm:text-[14px]";
const label =
  "mb-2 block text-[10px] uppercase tracking-[0.3em] text-muted";

export function ContactForm() {
  const [status, setStatus] = useState<Status>("idle");
  const [error, setError] = useState("");

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setStatus("sending");
    setError("");
    const form = e.currentTarget;
    const data = Object.fromEntries(new FormData(form));
    try {
      const res = await fetch("/api/contact", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(data),
      });
      if (!res.ok) {
        const j = (await res.json().catch(() => ({}))) as { error?: string };
        throw new Error(j.error ?? "something went wrong");
      }
      setStatus("sent");
      form.reset();
    } catch (err) {
      setStatus("error");
      setError(err instanceof Error ? err.message : "something went wrong");
    }
  }

  if (status === "sent") {
    return (
      <p className="text-[13px] leading-relaxed text-fg-soft sm:text-[14px]">
        Message received. We&apos;ll reply to the address you gave.
      </p>
    );
  }

  return (
    <form onSubmit={onSubmit} className="space-y-6" noValidate>
      <div>
        <label htmlFor="name" className={label}>
          Name
        </label>
        <input id="name" name="name" required maxLength={120} className={field} />
      </div>
      <div>
        <label htmlFor="email" className={label}>
          Email
        </label>
        <input
          id="email"
          name="email"
          type="email"
          required
          maxLength={200}
          className={field}
        />
      </div>
      <div>
        <label htmlFor="message" className={label}>
          Message
        </label>
        <textarea
          id="message"
          name="message"
          required
          maxLength={5000}
          rows={6}
          className={`${field} resize-none`}
        />
      </div>

      {/* Honeypot — hidden from humans, ignored by the server when filled. */}
      <input
        type="text"
        name="company"
        tabIndex={-1}
        autoComplete="off"
        aria-hidden="true"
        className="hidden"
      />

      <div className="flex items-center gap-4 pt-2">
        <button
          type="submit"
          disabled={status === "sending"}
          className="btn btn-primary disabled:cursor-not-allowed disabled:opacity-50"
        >
          {status === "sending" ? "Sending" : "Send"}
        </button>
        {status === "error" && (
          <span className="text-[12px] text-muted">{error}</span>
        )}
      </div>
    </form>
  );
}
