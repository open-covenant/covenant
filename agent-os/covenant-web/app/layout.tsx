import type { Metadata, Viewport } from "next";
import { Archivo, JetBrains_Mono } from "next/font/google";
import { RightRailProvider } from "@/lib/rightRail";
import { DemoBanner } from "./components/DemoBanner";
import { Shell } from "./components/Shell";
import "./globals.css";

const DEMO_MODE = process.env.NEXT_PUBLIC_DEMO_MODE === "1";

// The Covenant faces: Archivo on its width axis for display, JetBrains Mono
// for everything else.
const archivo = Archivo({
  subsets: ["latin"],
  axes: ["wdth"],
  weight: "variable",
  display: "swap",
  variable: "--font-archivo",
});

const jetbrains = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["300", "400", "500", "600"],
  display: "swap",
  variable: "--font-jetbrains",
});

export const viewport: Viewport = {
  themeColor: "#000000",
  colorScheme: "dark",
};

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
    <html lang="en" className={`${archivo.variable} ${jetbrains.variable}`}>
      <body>
        <RightRailProvider>
          <DemoBanner />
          <Shell>{children}</Shell>
        </RightRailProvider>
      </body>
    </html>
  );
}
