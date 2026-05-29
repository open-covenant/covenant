import { NextResponse } from "next/server";

// Delivery is via Resend's REST API (no SDK dependency — just fetch).
// Required env: RESEND_API_KEY, CONTACT_TO. Optional: CONTACT_FROM
// (defaults to Resend's onboarding sender so the route works before a
// verified domain is configured).
const FROM = process.env.CONTACT_FROM ?? "Covenant <onboarding@resend.dev>";
const MAX = { name: 120, email: 200, message: 5000 } as const;

function clean(v: unknown, max: number): string {
  return typeof v === "string" ? v.trim().slice(0, max) : "";
}

export async function POST(req: Request) {
  const key = process.env.RESEND_API_KEY;
  const to = process.env.CONTACT_TO;
  if (!key || !to) {
    return NextResponse.json({ error: "contact endpoint not configured" }, { status: 503 });
  }

  let body: unknown;
  try {
    body = await req.json();
  } catch {
    return NextResponse.json({ error: "invalid request" }, { status: 400 });
  }

  const b = body as Record<string, unknown>;
  // Honeypot — bots fill hidden fields, humans never see them.
  if (clean(b.company, 200)) {
    return NextResponse.json({ ok: true });
  }

  const name = clean(b.name, MAX.name);
  const email = clean(b.email, MAX.email);
  const message = clean(b.message, MAX.message);

  if (!name || !email || !message || !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) {
    return NextResponse.json({ error: "name, valid email, and message are required" }, { status: 400 });
  }

  const res = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: { Authorization: `Bearer ${key}`, "Content-Type": "application/json" },
    body: JSON.stringify({
      from: FROM,
      to: [to],
      reply_to: email,
      subject: `Covenant contact — ${name}`,
      text: `From: ${name} <${email}>\n\n${message}`,
    }),
  });

  if (!res.ok) {
    return NextResponse.json({ error: "delivery failed" }, { status: 502 });
  }

  return NextResponse.json({ ok: true });
}
