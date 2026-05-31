import type { MetadataRoute } from "next";

export default function manifest(): MetadataRoute.Manifest {
  return {
    name: "Covenant — open agent-native operating layer",
    short_name: "Covenant",
    description:
      "Covenant is the coordination layer for agentic software — host-level primitives for humans and agents.",
    start_url: "/",
    scope: "/",
    display: "standalone",
    background_color: "#030303",
    theme_color: "#030303",
    icons: [
      { src: "/icon.png", sizes: "512x512", type: "image/png", purpose: "any" },
      { src: "/icon-maskable.png", sizes: "512x512", type: "image/png", purpose: "maskable" },
      { src: "/apple-icon.png", sizes: "180x180", type: "image/png" },
    ],
  };
}
