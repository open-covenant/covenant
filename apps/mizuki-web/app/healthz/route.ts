export function GET() {
  return Response.json(
    { ok: true },
    {
      headers: {
        'cache-control': 'no-store',
      },
    },
  );
}
