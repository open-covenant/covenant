import { describe, expect, it } from "vitest";
import { PublicKey } from "@solana/web3.js";
import {
  decodePositionCreated,
  encodePositionCreatedLogLine,
  eventDiscriminator,
  extractPositionCreatedEvents,
  POSITION_CREATED_DISCRIMINATOR,
  type PositionCreatedEvent,
} from "./events.js";

const sample: PositionCreatedEvent = {
  owner: new PublicKey("So11111111111111111111111111111111111111112"),
  nonce: 7n,
  amount: 9_988_818_000_000n,
  weight: 4_994_409_000_000n,
  multiplierBps: 5000,
  lockEnd: 1_900_000_000n,
};

describe("PositionCreated event", () => {
  it('uses sha256("event:PositionCreated")[..8] as discriminator', () => {
    expect(POSITION_CREATED_DISCRIMINATOR).toEqual(
      eventDiscriminator("PositionCreated"),
    );
    expect(POSITION_CREATED_DISCRIMINATOR.length).toBe(8);
  });

  it("round-trips through a Program data log line", () => {
    const line = encodePositionCreatedLogLine(sample);
    expect(line.startsWith("Program data: ")).toBe(true);

    const events = extractPositionCreatedEvents([line]);
    expect(events).toHaveLength(1);
    const ev = events[0]!;
    expect(ev.owner.toBase58()).toBe(sample.owner.toBase58());
    expect(ev.nonce).toBe(sample.nonce);
    expect(ev.amount).toBe(sample.amount);
    expect(ev.weight).toBe(sample.weight);
    expect(ev.multiplierBps).toBe(sample.multiplierBps);
    expect(ev.lockEnd).toBe(sample.lockEnd);
  });

  it("ignores program logs, invoke lines, and foreign event data", () => {
    const noise = [
      "Program log: instruction: CreatePosition",
      "Program CstkpU2q9RngbHh21WVAYeQjbN9UWgcH9pAiQcMaEcED invoke [1]",
      `Program data: ${Buffer.from("not-an-anchor-event").toString("base64")}`,
      encodePositionCreatedLogLine(sample),
    ];
    const events = extractPositionCreatedEvents(noise);
    expect(events).toHaveLength(1);
    expect(events[0]!.amount).toBe(sample.amount);
  });

  it("rejects a right-length buffer with the wrong discriminator", () => {
    const buf = Buffer.alloc(8 + 32 + 8 + 8 + 16 + 2 + 8); // all-zero discriminator
    expect(decodePositionCreated(buf)).toBeNull();
  });

  it("returns empty for null / undefined / empty logs", () => {
    expect(extractPositionCreatedEvents(null)).toEqual([]);
    expect(extractPositionCreatedEvents(undefined)).toEqual([]);
    expect(extractPositionCreatedEvents([])).toEqual([]);
  });
});
