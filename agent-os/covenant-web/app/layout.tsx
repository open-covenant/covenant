import type { Metadata } from "next";
import { DemoBanner } from "./components/DemoBanner";
import { Shell } from "./components/Shell";
import "./globals.css";

export const metadata: Metadata = {
  title: "Covenant · Control Panel",
  description: "Your local control panel for Covenant. Send tasks, manage permissions, and check the activity log.",
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
