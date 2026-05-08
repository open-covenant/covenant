import { NextResponse, type NextRequest } from "next/server";

const DOCS_HOST = /^docs\./i;

export function middleware(req: NextRequest) {
  const host = req.headers.get("host") ?? "";
  const url = req.nextUrl;
  const path = url.pathname;
  const onDocsHost = DOCS_HOST.test(host);
  const proto =
    req.headers.get("x-forwarded-proto") ?? url.protocol.replace(":", "");

  if (onDocsHost) {
    if (path.startsWith("/docs")) {
      const cleaned = path.replace(/^\/docs/, "") || "/";
      return NextResponse.redirect(
        new URL(`${proto}://${host}${cleaned}${url.search}`),
        308,
      );
    }
    const rewritten = url.clone();
    rewritten.pathname = path === "/" ? "/docs" : `/docs${path}`;
    return NextResponse.rewrite(rewritten);
  }

  if (path === "/docs" || path.startsWith("/docs/")) {
    const cleanPath = path.replace(/^\/docs/, "") || "/";
    const apex = host.replace(/^www\./, "");
    return NextResponse.redirect(
      new URL(`${proto}://docs.${apex}${cleanPath}${url.search}`),
      308,
    );
  }

  return NextResponse.next();
}

export const config = {
  matcher: ["/((?!_next/|api/|.*\\..*).*)"],
};
