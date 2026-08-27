import { permanentRedirect } from "next/navigation";

// The equity firewall now lives with the spend escrow on the Robinhood Chain
// page; the two bounds are one story. Kept so the standalone link still lands.
export default function EquitiesPage() {
  permanentRedirect("/robinhood");
}
