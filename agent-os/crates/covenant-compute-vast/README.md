# Covenant Compute Vast adapter

This crate treats Vast offer-search filters as a request, not as evidence. Every
returned offer must independently report all of the following before the
adapter can create a billable instance:

- `verification == "verified"`
- `reliability >= 0.99`
- `rentable == true` and `rented == false`
- exactly one NVIDIA GPU on an AMD64 host
- at least one direct port
- `cuda_max_good >= 12.4`

The adapter retains those facts on `Offer` and re-runs the search immediately
before creation. Quote identity, machine, GPU, memory, and price must still
match the accepted `OfferQuote`.

The only registered workspace image is the immutable
`docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10`
AMD64 image. Its OCI configuration declares CUDA 12.4.1 and
`NVIDIA_REQUIRE_CUDA=cuda>=12.4`, so other workspace digests fail before any
provider request until their compatibility is reviewed and registered.

Vast exposes `cuda_max_good` as a numeric major/minor value derived from the
host driver. It does not prove the container can initialize CUDA, and the offer
response cannot prove that the eventual Jupyter process will boot. Likewise,
`direct_port_count` proves offer-time port capacity, not the post-allocation
mapping. After creation, the adapter therefore requires the exact instance,
image, `jupyter_direct` runtime, GPU, machine, offer, price, public IP, 8080 port
mapping, and Jupyter token before returning an access URL. Missing facts remain
in provisioning; contradictory or malformed facts fail closed.

API field semantics:

- [Search offers](https://docs.vast.ai/api-reference/search/search-offers)
- [Show instances](https://docs.vast.ai/api-reference/instances/show-instances)
