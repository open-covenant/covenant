import { beforeEach, describe, expect, it, vi } from "vitest";
import { GET } from "../[...path]/route";
import { fetchComputePage } from "../_compute";
import { fetchPage } from "../_page";
import { renderCard } from "../_card";

vi.mock("../_compute", async (original) => ({ ...await original<typeof import("../_compute")>(), fetchComputePage: vi.fn() }));
vi.mock("../_page", async (original) => ({ ...await original<typeof import("../_page")>(), fetchPage: vi.fn() }));
vi.mock("../_card", () => ({ renderCard: vi.fn(async () => new Response(new Uint8Array([137, 80, 78, 71]))) }));

const source = (title: string) => ({
  status: 200, host: "compute-api.opencovenant.org", path: "/prices",
  headers: new Headers({ "content-type": "text/html" }),
  html: `<title>${title}</title><meta property="og:url" content="https://untrusted.test/">`,
});
const request = (path: string[]) => GET(new Request(`https://opencovenant.org/og/${path.join("/")}`, {
  headers: { host: "untrusted.test", "x-forwarded-host": "untrusted.test" },
}), { params: Promise.resolve({ path }) });

beforeEach(() => vi.clearAllMocks());

describe("Compute card route", () => {
  it("uses the shared renderer and trusted canonical URL regardless of page/Host metadata", async () => {
    vi.mocked(fetchComputePage).mockResolvedValue(source("GPU price history · Covenant Compute"));
    const response = await request(["compute-api", "prices.png"]);
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("image/png");
    expect(fetchPage).not.toHaveBeenCalled();
    expect(fetchComputePage).toHaveBeenCalledWith(["prices"]);
    expect(renderCard).toHaveBeenCalledWith({
      title: "GPU price history", section: "Compute", url: "https://compute-api.opencovenant.org/prices", key: "/compute-api/prices",
    });
  });

  it("keeps existing site routes on the existing local-page reader", async () => {
    vi.mocked(fetchPage).mockResolvedValue({ ...source("Covenant SDK"), host: "opencovenant.org", path: "/sdk" });
    expect((await request(["sdk.png"])).status).toBe(200);
    expect(fetchPage).toHaveBeenCalled();
    expect(fetchComputePage).not.toHaveBeenCalled();
  });

  it("maps the reserved index card to the Compute root", async () => {
    vi.mocked(fetchComputePage).mockResolvedValue(source("Covenant Compute"));
    expect((await request(["compute-api", "index.png"])).status).toBe(200);
    expect(renderCard).toHaveBeenCalledWith(expect.objectContaining({ url: "https://compute-api.opencovenant.org/" }));
  });

  it("accepts an 80-character page segment plus image suffix but rejects longer paths", async () => {
    vi.mocked(fetchComputePage).mockResolvedValue(source("Future page"));
    expect((await request(["compute-api", `${"a".repeat(80)}.png`])).status).toBe(200);
    expect(fetchComputePage).toHaveBeenCalledWith(["a".repeat(80)]);
    vi.clearAllMocks();
    expect((await request(["compute-api", `${"a".repeat(81)}.png`])).status).toBe(404);
    expect(fetchComputePage).not.toHaveBeenCalled();
  });

  it("refuses traversal without fetching or rendering", async () => {
    expect((await request(["compute-api", "..", "prices.png"])).status).toBe(404);
    expect(fetchComputePage).not.toHaveBeenCalled();
    expect(renderCard).not.toHaveBeenCalled();
  });

  it("does not render or cache a missing page", async () => {
    vi.mocked(fetchComputePage).mockResolvedValue({ ...source("Missing"), status: 404 });
    const response = await request(["compute-api", "missing.png"]);
    expect(response.status).toBe(404);
    expect(renderCard).not.toHaveBeenCalled();
  });
});
