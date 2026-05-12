import type { Metadata } from "next";
import { Shell } from "./components/Shell";
import "./globals.css";

export const metadata: Metadata = {
  title: "covenant · operator console",
  description: "Local control plane for the covenant agent operating layer.",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>
        <Shell>{children}</Shell>
      </body>
    </html>
  );
}
