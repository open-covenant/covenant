import Image from "next/image";

const RELEASE_DATE = "13.05.2026";
const X_URL = "https://x.com/OpenCovenant";
const GITHUB_URL = "https://github.com/open-covenant/covenant";

function XIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path d="M17.53 3H20.5l-6.49 7.41L21.75 21h-6.18l-4.84-6.34L5.16 21H2.18l6.94-7.93L1.75 3h6.34l4.38 5.79L17.53 3Zm-1.08 16.2h1.71L7.66 4.7H5.83l10.62 14.5Z" />
    </svg>
  );
}

function GithubIcon({ className }: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      className={className}
      fill="currentColor"
    >
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0 1 12 6.844c.85.004 1.705.115 2.504.337 1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.203 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.31.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0 0 22 12.017C22 6.484 17.522 2 12 2Z"
      />
    </svg>
  );
}

export default function Page() {
  return (
    <main className="relative min-h-screen overflow-hidden bg-black">
      <header className="pointer-events-none absolute inset-x-0 top-0 z-10 flex justify-center pt-10 sm:pt-14">
        <Image
          src="/logo.svg"
          alt="covenant"
          width={255}
          height={54}
          priority
          className="pointer-events-auto h-10 w-auto opacity-95 sm:h-12"
        />
      </header>

      <nav className="absolute top-8 right-6 z-10 flex items-center gap-4 sm:top-10 sm:right-10">
        <a
          href={X_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on X"
          className="text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <XIcon className="h-5 w-5" />
        </a>
        <a
          href={GITHUB_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-label="Covenant on GitHub"
          className="text-neutral-400 transition-colors hover:text-neutral-50"
        >
          <GithubIcon className="h-5 w-5" />
        </a>
      </nav>

      <section className="relative flex min-h-screen flex-col items-center justify-center px-6 text-center">
        <div className="absolute inset-0 -z-0">
          <Image
            src="/hero.png"
            alt=""
            fill
            priority
            sizes="100vw"
            className="object-cover opacity-70"
          />
          <div className="absolute inset-0 bg-gradient-to-b from-black/40 via-black/20 to-black" />
        </div>

        <div className="relative z-10 flex flex-col items-center gap-8 pt-32">
          <p className="text-xs tracking-[0.4em] text-neutral-400 uppercase">
            Release
          </p>
          <p className="font-mono text-3xl font-light tracking-tight text-neutral-50 sm:text-5xl">
            {RELEASE_DATE}
          </p>
        </div>

        <footer className="absolute inset-x-0 bottom-8 flex justify-center text-[11px] tracking-widest text-neutral-500 uppercase">
          covenant — an open agent-native operating layer
        </footer>
      </section>
    </main>
  );
}
