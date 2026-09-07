# Outreach note to LONG (draft, not sent)

Ask: documented route-signing access for Covenant Desk.
Channel to be decided by the operator. Nothing here has been sent.

---

Covenant Desk is a local execution desk for Robinhood Chain stock tokens and
the tokens paired with them, and it already prices and fills LONG pools by
encoding its own Uniswap v4 route. We would rather send those trades through
your router at `0x6F6F5E1b4669C2e1553E65e6162D9E60172bb7Fe`, so your fee
recipients and fee basis points stay exactly as your frontend sets them, and
your volume stays attributed to LONG. What we need is an endpoint that returns
a signed route for a given input token, output token, amount, recipient and
deadline, along with whatever integrator key you want us to carry and any rate
limit you want us to respect. If route signing is not something you open up,
tell us and we will keep routing around it, and we will still credit LONG as
the venue in everything the desk reports.
