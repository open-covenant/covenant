import type { Metadata } from "next";
import { RightRailProvider } from "@/lib/rightRail";
import { DemoBanner } from "./components/DemoBanner";
import { Shell } from "./components/Shell";
import "./globals.css";

const DEMO_MODE = process.env.NEXT_PUBLIC_DEMO_MODE === "1";

export const metadata: Metadata = DEMO_MODE
  ? {
      title: "Covenant · Coding Sandbox",
      description:
        "Describe an app or script and watch a Covenant agent write, run, and verify the code in a live sandbox.",
    }
  : {
      title: "Covenant · Control Panel",
      description:
        "Your local control panel for Covenant. Send tasks, manage permissions, and check the activity log.",
    };

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>
        <RightRailProvider>
          <DemoBanner />
          <Shell>{children}</Shell>
        </RightRailProvider>
      </body>
    </html>
  );
}
