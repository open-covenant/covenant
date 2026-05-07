import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "covenant",
  description: "open agent-native operating layer",
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
