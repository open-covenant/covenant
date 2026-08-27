export function GET() {
  return Response.json(
    {
      ok: true,
      buildId: process.env.NEXT_PUBLIC_MIZUKI_BUILD_ID?.trim() || 'development',
    },
    {
      headers: {
        'cache-control': 'no-store',
      },
    },
  );
}
