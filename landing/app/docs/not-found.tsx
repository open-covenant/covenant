import type { Metadata } from "next";
import Link from "next/link";

export const metadata: Metadata = {
  title: "Page not found",
  robots: { index: false, follow: false },
};

export default function DocsNotFound() {
  return (
    <>
      <h1>Page not found</h1>
      <p>
        This documentation page could not be found. Return to the{" "}
        <Link href="/">documentation home</Link>, or visit the{" "}
        <a href="https://opencovenant.org">main site</a>.
      </p>
    </>
  );
}
