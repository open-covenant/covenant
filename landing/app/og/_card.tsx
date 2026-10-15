import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { ImageResponse } from "next/og";
import { fitTitle, TITLE_LEADING, TITLE_TRACKING, wordSpacing } from "./_fit";

// The page card: Covenant lockup top left, the page's section top right, the
// title in the display face bottom left over a strip of the field, and the
// page's address beneath it. Same frame as the home share card.

export const CARD_WIDTH = 1200;
export const CARD_HEIGHT = 630;

const ASSETS = join(process.cwd(), "app/og/_assets");
const FIELDS = ["field-1.png", "field-2.png", "field-3.png", "field-4.png"];

const TEXT = "#F3F1FA";
const MUTED = "#A49EC0";
const MARGIN = 64;
const TITLE_WIDTH = CARD_WIDTH - MARGIN * 2;

type Assets = { display: Buffer; mono: Buffer; mark: string; fields: string[] };
let assets: Promise<Assets> | null = null;

function loadAssets(): Promise<Assets> {
  assets ??= (async () => {
    const [display, mono, markSvg, ...fields] = await Promise.all([
      readFile(join(ASSETS, "archivo-expanded-600.ttf")),
      readFile(join(ASSETS, "jetbrains-mono-500.ttf")),
      readFile(join(process.cwd(), "public/covenant-mark.svg"), "utf8"),
      ...FIELDS.map((f) => readFile(join(ASSETS, f))),
    ]);
    return {
      display,
      mono,
      mark: `data:image/svg+xml;base64,${Buffer.from(markSvg).toString("base64")}`,
      fields: fields.map((f) => `data:image/png;base64,${f.toString("base64")}`),
    };
  })().catch((err) => {
    assets = null;
    throw err;
  });
  return assets;
}

// Same page, same field; neighbouring pages usually differ.
function fieldFor(key: string, count: number): number {
  let h = 2166136261;
  for (let i = 0; i < key.length; i++) h = Math.imul(h ^ key.charCodeAt(i), 16777619);
  return (h >>> 0) % count;
}

export function displayUrl(url: string): string {
  return url.replace(/^https?:\/\//, "").replace(/\/$/, "");
}

export async function renderCard({ title, section, url, key }: { title: string; section?: string; url: string; key: string }) {
  const { display, mono, mark, fields } = await loadAssets();
  const fitted = fitTitle(title, { width: TITLE_WIDTH });
  const label = { fontFamily: "JetBrains Mono", fontSize: 14, letterSpacing: 3.4, textTransform: "uppercase" as const };

  return new ImageResponse(
    (
      <div style={{ width: CARD_WIDTH, height: CARD_HEIGHT, display: "flex", position: "relative", background: "#000000" }}>
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img src={fields[fieldFor(key, fields.length)]} width={CARD_WIDTH} height={CARD_HEIGHT} alt="" style={{ position: "absolute", left: 0, top: 0 }} />
        <div
          style={{
            position: "absolute",
            left: 0,
            top: 0,
            width: CARD_WIDTH,
            height: CARD_HEIGHT,
            backgroundImage: "linear-gradient(to right, rgba(0,0,0,0.94) 0%, rgba(0,0,0,0.62) 36%, rgba(0,0,0,0) 72%)",
          }}
        />
        <div
          style={{
            position: "absolute",
            left: 0,
            top: 0,
            width: CARD_WIDTH,
            height: 200,
            backgroundImage: "linear-gradient(to bottom, rgba(0,0,0,0.9) 0%, rgba(0,0,0,0.55) 50%, rgba(0,0,0,0) 100%)",
          }}
        />
        <div
          style={{
            position: "absolute",
            left: 0,
            bottom: 0,
            width: CARD_WIDTH,
            height: 380,
            backgroundImage: "linear-gradient(to top, rgba(0,0,0,0.9) 0%, rgba(0,0,0,0.55) 45%, rgba(0,0,0,0) 100%)",
          }}
        />

        <div style={{ position: "absolute", left: MARGIN, top: 52, display: "flex", alignItems: "center" }}>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img src={mark} width={30} height={38} alt="" />
          <div style={{ ...label, color: TEXT, marginLeft: 14 }}>Covenant</div>
        </div>
        {section ? (
          <div style={{ ...label, position: "absolute", right: MARGIN - 3.4, top: 63, color: MUTED }}>{section}</div>
        ) : null}

        <div style={{ position: "absolute", left: MARGIN - fitted.size * 0.04, bottom: 112, display: "flex", flexDirection: "column" }}>
          {fitted.lines.map((line, i) => (
            <div
              key={i}
              style={{
                display: "flex",
                fontFamily: "Archivo Expanded",
                fontSize: fitted.size,
                lineHeight: TITLE_LEADING,
                letterSpacing: fitted.size * TITLE_TRACKING,
                color: TEXT,
              }}
            >
              {wordSpacing(line).map(({ word, after }, j) => (
                <div key={j} style={{ marginRight: after * fitted.size, whiteSpace: "nowrap" }}>
                  {word}
                </div>
              ))}
            </div>
          ))}
        </div>
        <div style={{ ...label, position: "absolute", left: MARGIN, bottom: 52, fontSize: 13, letterSpacing: 3.1, color: "rgba(243,241,250,0.82)" }}>
          {displayUrl(url)}
        </div>
      </div>
    ),
    {
      width: CARD_WIDTH,
      height: CARD_HEIGHT,
      fonts: [
        { name: "Archivo Expanded", data: display, weight: 600, style: "normal" },
        { name: "JetBrains Mono", data: mono, weight: 500, style: "normal" },
      ],
    },
  );
}
