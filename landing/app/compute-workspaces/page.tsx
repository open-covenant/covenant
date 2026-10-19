import type { Metadata } from "next";
import { pageMetadata } from "../_metadata";
import { SiteFooter } from "../SiteFooter";
import { SiteHeader } from "../SiteHeader";
import { workspaceRelease as release } from "./_release";

export const metadata: Metadata = pageMetadata(
  "/compute-workspaces",
  { title: "Encrypted Compute workspaces", section: "Compute" },
  {
    title: "Encrypted workspaces: Covenant Compute",
    description: "Save encrypted, versioned checkpoints independently of a GPU lease. Keep the encryption key on your client and restore the exact saved snapshot for a later lease.",
    ...(!release.accepted ? { robots: { index: false, follow: false } } : {}),
  },
);

const eyebrow = "font-mono text-[11px] uppercase tracking-[0.3em] text-muted";
const heading = "text-[13px] uppercase tracking-[0.25em] text-fg sm:text-[14px]";
const paragraph = "text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]";
const link = "underline decoration-line-strong underline-offset-4 transition-colors hover:decoration-fg-soft hover:text-fg";

const saveExample = `# Use your existing buyer identity and accepted coordinator endpoint.
# Copy checkpoint files from the source lease through authenticated SSH,
# then archive the directory locally before saving it.
tar -cf checkpoint.tar -C ./checkpoint-files .

# Save the workspace UUID. Reuse it for every command below.
covenant-compute workspace create training-checkpoint --id <workspace-uuid>
covenant-compute workspace save <workspace-uuid> checkpoint.tar
covenant-compute workspace status <workspace-uuid>

# An uncertain upload or commit keeps its original ciphertext and identity.
covenant-compute workspace retry <workspace-uuid>`;

const restoreExample = `# The destination file must not already exist.
# --lease checks ownership, the live paid window and the workspace trust floor.
covenant-compute workspace restore <workspace-uuid> restored.tar \\\n  --lease <later-lease-uuid>

# Compare archive digests before transferring the restored file.
shasum -a 256 checkpoint.tar restored.tar

# Transfer restored.tar to the later lease through authenticated SSH.
# The restore command itself writes the file to your local client.`;

const backupExample = `# This private backup contains the encryption key and exact version pin.
covenant-compute workspace backup <workspace-uuid> private-backup.json

# On another client with the same buyer identity and coordinator endpoint:
covenant-compute workspace import private-backup.json

# Delete the workspace when it is no longer needed.
covenant-compute workspace delete <workspace-uuid>`;

function CodeBlock({ label, children }: { label: string; children: string }) {
  return <div className="mt-6 overflow-hidden border border-line/80 bg-raised/30">
    <div className="border-b border-line/80 px-5 py-3 font-mono text-[10px] uppercase tracking-[0.2em] text-muted sm:px-6">{label}</div>
    <pre className="overflow-x-auto px-5 py-6 text-[11px] leading-[1.9] text-fg-soft sm:px-6 sm:text-[12px]" tabIndex={0} aria-label={label}><code>{children}</code></pre>
  </div>;
}

export default function ComputeWorkspacesPage() {
  const accepted = release.accepted && release.acceptance?.crossLeaseVerified && release.acceptance.cleanupVerified && release.acceptance;
  return <main id="main-content" className="relative min-h-screen overflow-x-hidden bg-ground">
    <SiteHeader />
    <div className="page-container">
      <p className="mb-10 text-[11px] uppercase tracking-[0.4em] text-muted sm:mb-14">Compute</p>
      <h1 className="type-display max-w-3xl text-balance text-[2.2rem] text-fg sm:text-[2.6rem]">Encrypted checkpoints for Covenant Compute.</h1>
      <p className="mt-6 max-w-2xl text-pretty text-[15px] leading-[1.7] text-fg-soft sm:text-base">Save a checkpoint before closing your GPU lease. Restore the same file for your next lease. Your client encrypts the snapshot, and you retain the key.</p>
      <div className="mt-7 flex flex-wrap items-center gap-x-7 gap-y-2"><a href="#workflow" className={`inline-flex min-h-11 items-center text-[13px] text-fg ${link}`}>See the workflow <span aria-hidden="true" className="ml-2">↓</span></a><a href="#verification" className="inline-flex min-h-11 items-center text-[13px] text-muted transition-colors hover:text-fg">Inspect the evidence <span aria-hidden="true" className="ml-2">↓</span></a></div>
      {!release.accepted && <aside className="mt-8 max-w-3xl border-l border-line-strong pl-5" aria-label="Unpublished release preview"><p className={eyebrow}>Release preview · not published</p><p className={`mt-2 ${paragraph}`}>Implementation is prepared. Storage acceptance, released-client recovery and a verified transfer between real GPU leases are required before public release.</p></aside>}
      <div className="mt-10 flex flex-wrap gap-x-6 gap-y-3 border-y border-line/80 py-5 text-[12px] text-muted sm:mt-14"><span>Client-side encryption</span><span>Versioned snapshots</span><span>Persistent recovery</span></div>

      <section aria-labelledby="how-heading" className="mt-14 sm:mt-20">
        <h2 id="how-heading" className={heading}>Save a checkpoint. Restore the exact version.</h2>
        <ol className="mt-8 grid gap-8 sm:grid-cols-3">{[
          ["01", "Save", "Your client encrypts a local file and uploads the ciphertext directly to private object storage. Archive a directory first to capture its files in one snapshot."],
          ["02", "Commit", "Covenant checks the stored ciphertext’s size and SHA-256 digest before advancing the workspace version. The client retains the version it accepted."],
          ["03", "Restore", "Your client requests its pinned snapshot, checks the ciphertext and decrypts it locally. Transfer the recovered file to the next lease through an authenticated SSH connection."],
        ].map(([number,title,body])=><li key={number}><p className={eyebrow}>{number}</p><h3 className="mt-3 text-[15px] text-fg">{title}</h3><p className={`mt-3 ${paragraph}`}>{body}</p></li>)}</ol>
      </section>

      <section id="workflow" aria-labelledby="workflow-heading" className="mt-14 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="workflow-heading" className={heading}>From one lease to the next</h2>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Use the same buyer identity and coordinator endpoint throughout. Copy the checkpoint from your source lease to your client before saving it. Each save captures a file at that moment; subsequent changes need another save.</p>
        {release.installation ? <CodeBlock label="Released client · terminal">{release.installation}</CodeBlock> : <p className={`mt-5 max-w-2xl border-l border-line-strong pl-5 ${paragraph}`}>Pinned client installation instructions will be added after release acceptance. The commands below describe the prepared interface.</p>}
        <CodeBlock label="Archive and save · terminal">{saveExample}</CodeBlock>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>An interrupted operation retains its ciphertext and upload identity. Use <code>workspace retry</code> to resolve that operation before saving another version. Keep the client’s workspace state directory so its key, version pin and pending operation survive a restart.</p>
        <CodeBlock label="Restore for a later lease · terminal">{restoreExample}</CodeBlock>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The <code>--lease</code> option authorizes a live lease owned by the same buyer and checks its trust level. The restored archive is created on your client; file transfer and unpacking on the rented machine are separate steps under your control.</p>
        <p className={`mt-4 max-w-2xl ${paragraph}`}>Saving or restoring a snapshot does not create a rental or transfer funds. GPU leases retain their existing prices, settlement and close procedures.</p>
      </section>

      <section id="verification" aria-labelledby="verification-heading" className="mt-14 scroll-mt-28 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="verification-heading" className={heading}>Release evidence</h2>
        {accepted ? <>
          <p className={`mt-5 max-w-2xl ${paragraph}`}>A synthetic checkpoint was created on an NVIDIA L40S lease, copied to the client, encrypted and saved. After that lease closed, the client restored the archive and transferred it to a second rented instance. Its SHA-256 digest matched exactly. Both instances used the same physical L40S host.</p>
          <p className={`mt-4 max-w-2xl ${paragraph}`}>A separate recovery test interrupted the released client during save, then committed the original ciphertext and upload identity after restart. Both leases settled on Solana, their rented instances were removed, and cleanup replaced the stored ciphertext with verified empty markers. These tests establish file persistence and recovery; they do not demonstrate a resumed training run or hardware attestation.</p>
          <dl className="mt-7 divide-y divide-line/80 border-y border-line/80">{[["Verified",accepted.checkedAt],["Storage",accepted.storageProvider],["Source lease",accepted.sourceLeaseId],["Later lease",accepted.targetLeaseId],["Archive SHA-256",accepted.archiveSha256],["Ciphertext SHA-256",accepted.ciphertextSha256]].map(([name,value])=><div key={name} className="grid gap-2 py-4 sm:grid-cols-[180px_1fr] sm:gap-7"><dt className="text-[12px] text-muted">{name}</dt><dd className="break-all font-mono text-[12px] leading-[1.8] text-fg-soft">{value}</dd></div>)}</dl>
          <a href={accepted.evidenceUrl} className={`mt-4 inline-flex min-h-11 items-center text-[13px] ${link}`}>Inspect the acceptance record ↗</a>
          <a href="/compute/workspaces/cross-lease-acceptance.json" className={`ml-6 mt-4 inline-flex min-h-11 items-center text-[13px] ${link}`}>Raw acceptance data ↗</a>
        </> : <p className={`mt-5 max-w-2xl border-l border-line-strong pl-5 ${paragraph}`}>Production acceptance is pending. This section will link the provider’s immutable-write and checksum checks, released-client save and recovery, matching archive digests, verified lease access and cleanup. Local tests alone do not establish these production results.</p>}
      </section>

      <section aria-labelledby="protection-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="protection-heading" className={heading}>Encryption, ownership and version checks</h2>
        <div className="mt-7 grid gap-8 sm:grid-cols-2">{[
          ["The key stays with your client", "Snapshots use authenticated encryption. The coordinator and object store receive ciphertext and metadata. Keep a private backup of the key; Covenant cannot reconstruct a lost key."],
          ["The buyer authorizes access", "Workspace requests are signed by the owner. Short-lived storage grants cover a specific operation. A lease restore also checks ownership, its remaining paid window and whether closing has begun."],
          ["Your client pins the saved version", "Restore checks the exact snapshot your client accepted. A different version or digest is refused, and an existing destination file is preserved. Older backups cannot replace a newer local version pin."],
          ["Hardware trust is checked separately", "Current lease hardware qualifies only for the open trust level. A workspace with a stronger requirement can be recovered locally by its owner; release to lease hardware requires verified evidence that satisfies that requirement."],
        ].map(([title,body])=><div key={title}><h3 className="text-[15px] text-fg">{title}</h3><p className={`mt-3 ${paragraph}`}>{body}</p></div>)}</div>
      </section>

      <section aria-labelledby="backup-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="backup-heading" className={heading}>Keep a private backup</h2>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>The workspace backup contains its encryption key and exact version pin. Store it privately, separately from the rented machine. Restore it on a client using the same buyer identity and coordinator endpoint.</p>
        <CodeBlock label="Backup, import and deletion · terminal">{backupExample}</CodeBlock>
        <p className={`mt-5 max-w-2xl ${paragraph}`}>Deletion stops new workspace access. After outstanding grants expire, cleanup replaces the stored ciphertext with an empty marker that prevents a delayed upload from recreating it. The marker is retained. Expired uploads and replaced snapshots follow the same cleanup process.</p>
        <p className={`mt-4 max-w-2xl ${paragraph}`}>A previously issued ciphertext URL can remain usable until it expires. Deletion does not erase files or private backups already saved on your own machines.</p>
      </section>

      <section aria-labelledby="limits-heading" className="mt-14 border-t border-line/80 pt-9 sm:mt-20 sm:pt-11">
        <h2 id="limits-heading" className={heading}>Snapshot limits</h2>
        <dl className="mt-7 divide-y divide-line/80 border-y border-line/80">{[["Snapshot", "One file per saved version; archive directories before saving"],["Current capacity", "1 GiB shared across the deployment, including ciphertext overhead, pending uploads and unconfirmed cleanup"],["Object quota", "4,096 lifetime object allocations across the deployment, including retained cleanup markers; retries reuse their allocation"],["Versions", "Restore the latest committed snapshot pinned by your client"],["Transfer", "Client-managed copying over authenticated SSH"]].map(([name,value])=><div key={name} className="grid gap-2 py-4 sm:grid-cols-[180px_1fr] sm:gap-7"><dt className="text-[12px] text-muted">{name}</dt><dd className="text-[13px] leading-[1.8] text-fg-soft">{value}</dd></div>)}</dl>
      </section>
      <div className="mt-14 flex flex-wrap gap-x-6 gap-y-3 border-t border-line/80 pt-7 sm:mt-20">{[{label:"Compute",href:"/compute"},{label:"Compute prices",href:"/compute-prices"},{label:"Compute status",href:"/compute-status"},...(release.documentationUrl?[{label:"Workspace documentation",href:release.documentationUrl}]:[]),...(release.sourceUrl?[{label:"Implementation source",href:release.sourceUrl}]:[])].map(({label,href})=><a key={href} href={href} className="inline-flex min-h-11 items-center text-[11px] uppercase tracking-[0.2em] text-muted transition-colors hover:text-fg">{label}<span aria-hidden="true" className="ml-2">→</span></a>)}</div>
    </div><SiteFooter className="pb-8" />
  </main>;
}
