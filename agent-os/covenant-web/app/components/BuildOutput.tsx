"use client";

import { useState } from "react";
import type { BuildFile } from "@/lib/api";

// Renders what a run built: a file tree + the selected file's contents, plus a
// live Preview for static HTML (sandboxed iframe — untrusted code can run
// scripts but can't reach this origin, cookies, or storage).
export function BuildOutput({ files }: { files: BuildFile[] }) {
  const html = files.find((f) => f.path.toLowerCase().endsWith(".html"));
  const [sel, setSel] = useState(0);
  const [tab, setTab] = useState<"files" | "preview">(html ? "preview" : "files");

  if (files.length === 0) return null;
  const active = files[Math.min(sel, files.length - 1)];

  return (
    <section className="build-output">
      <div className="bo-head">
        <p className="eyebrow">build output · {files.length} files</p>
        {html && (
          <div className="bo-tabs">
            <button
              type="button"
              className={tab === "preview" ? "on" : ""}
              onClick={() => setTab("preview")}
            >
              Preview
            </button>
            <button
              type="button"
              className={tab === "files" ? "on" : ""}
              onClick={() => setTab("files")}
            >
              Files
            </button>
          </div>
        )}
      </div>

      {tab === "preview" && html ? (
        <iframe
          className="bo-preview"
          // Untrusted output: allow scripts (the app needs them) but NOT
          // same-origin, so it can't touch this page, cookies, or storage.
          sandbox="allow-scripts allow-pointer-lock"
          srcDoc={html.content}
          title="build preview"
        />
      ) : (
        <div className="bo-body">
          <ul className="bo-tree">
            {files.map((f, i) => {
              const depth = f.path.split("/").length - 1;
              const name = f.path.split("/").pop() ?? f.path;
              return (
                <li key={f.path}>
                  <button
                    type="button"
                    className={i === sel ? "on" : ""}
                    style={{ paddingLeft: 10 + depth * 12 }}
                    onClick={() => {
                      setSel(i);
                      setTab("files");
                    }}
                    title={f.path}
                  >
                    {name}
                  </button>
                </li>
              );
            })}
          </ul>
          <div className="bo-file">
            <div className="bo-file-head">
              <span className="path">{active.path}</span>
              {active.truncated && <span className="trunc">truncated</span>}
            </div>
            <pre>
              <code>{active.content}</code>
            </pre>
          </div>
        </div>
      )}

      <style jsx>{`
        .build-output {
          margin: 24px 0;
          border: 1px solid var(--border);
          border-radius: 8px;
          background: var(--panel);
          overflow: hidden;
        }
        .bo-head {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 12px;
          padding: 10px 14px;
          border-bottom: 1px solid var(--border);
          background: #0a0a0a;
        }
        .bo-head .eyebrow {
          margin: 0;
        }
        .bo-tabs {
          display: flex;
          gap: 4px;
        }
        .bo-tabs button {
          padding: 3px 12px;
          border: 1px solid var(--border);
          border-radius: 999px;
          background: transparent;
          color: var(--dim);
          font-size: 11px;
          cursor: pointer;
        }
        .bo-tabs button.on {
          color: var(--fg);
          border-color: var(--fg);
        }
        .bo-preview {
          display: block;
          width: 100%;
          height: 460px;
          border: 0;
          background: #fff;
        }
        .bo-body {
          display: grid;
          grid-template-columns: minmax(140px, 220px) minmax(0, 1fr);
          min-height: 260px;
          max-height: 460px;
        }
        .bo-tree {
          margin: 0;
          padding: 8px 0;
          list-style: none;
          border-right: 1px solid var(--border);
          overflow: auto;
          background: #070707;
        }
        .bo-tree button {
          display: block;
          width: 100%;
          padding: 4px 10px;
          border: 0;
          background: transparent;
          color: var(--dim);
          font-family: var(--font-mono);
          font-size: 12px;
          text-align: left;
          white-space: nowrap;
          overflow: hidden;
          text-overflow: ellipsis;
          cursor: pointer;
        }
        .bo-tree button:hover {
          color: var(--fg);
        }
        .bo-tree button.on {
          color: var(--fg);
          background: var(--panel);
        }
        .bo-file {
          display: flex;
          flex-direction: column;
          min-width: 0;
          overflow: hidden;
        }
        .bo-file-head {
          display: flex;
          align-items: center;
          justify-content: space-between;
          gap: 10px;
          padding: 8px 14px;
          border-bottom: 1px solid var(--border);
          color: var(--muted);
          font-family: var(--font-mono);
          font-size: 11px;
        }
        .bo-file-head .trunc {
          color: #d4a017;
          letter-spacing: 0.04em;
        }
        .bo-file pre {
          margin: 0;
          padding: 12px 14px;
          overflow: auto;
        }
        .bo-file code {
          color: var(--fg);
          font-family: var(--font-mono);
          font-size: 12px;
          line-height: 1.5;
          white-space: pre;
        }
        @media (max-width: 720px) {
          .bo-body {
            grid-template-columns: 1fr;
            max-height: none;
          }
          .bo-tree {
            border-right: 0;
            border-bottom: 1px solid var(--border);
            max-height: 140px;
          }
        }
      `}</style>
    </section>
  );
}
