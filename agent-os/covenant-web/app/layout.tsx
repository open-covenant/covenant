import type { Metadata } from "next";
import { DemoBanner } from "./components/DemoBanner";
import { Shell } from "./components/Shell";
import "./globals.css";

export const metadata: Metadata = {
  title: "Covenant · Operator Console",
  description: "The local control plane for Covenant. Dispatch intents, manage capabilities, and verify the audit chain.",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>
        <DemoBanner />
        <Shell>{children}</Shell>
      </body>
    </html>
  );
}
