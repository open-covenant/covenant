export function internalSiteUrl(path: string): URL {
  if (!path.startsWith("/")) throw new Error("internal site path must be absolute");

  const configured =
    process.env.COVENANT_INTERNAL_SITE_URL ||
    (process.env.VERCEL_URL ? `https://${process.env.VERCEL_URL}` : undefined) ||
    process.env.NEXT_PUBLIC_SITE_URL;
  const base = configured || `http://127.0.0.1:${process.env.PORT || "3000"}`;
  const baseUrl = new URL(base);
  if (
    !["http:", "https:"].includes(baseUrl.protocol) ||
    baseUrl.username ||
    baseUrl.password
  ) {
    throw new Error("invalid configured site URL");
  }
  const url = new URL(path, baseUrl);
  if (url.origin !== baseUrl.origin) throw new Error("internal site path changed origin");
  return url;
}
