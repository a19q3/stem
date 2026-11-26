# Does near-quadratic broadcast apply at Stem committee sizes?

The authors' November 11 paper specifies one-bit authenticated broadcast with a designated sender, synchrony, a PKI, ideal signatures and binding vector commitments. Its communication bound counts bits sent by correct parties in the worst case. Section 3 implements the commitments with logarithmic-size openings. The October explanatory article implements a different, intermediate protocol. [Paper, Protocol 1 and Claim 14](https://opencryptology.org/2026/2504/versions/20261111T200107Z.pdf) · [Authors’ explanation](https://decentralizedthoughts.github.io/2026-10-23-subcubic-broadcast/)

With the published Ladder schedule, k = log₂(n), R = 8k + 8 and E = (k + 1)R(2k + 3). Protocol 1 runs Dolev–Strong when t < E. The replay enumerates every legal fault bound at each requested size.

| Parties | Legal fault bounds checked | E | Branch selected throughout |
| ---: | ---: | ---: | --- |
| 16 | 16 | 2,200 | Dolev–Strong |
| 64 | 64 | 5,880 | Dolev–Strong |
| 128 | 128 | 8,704 | Dolev–Strong |
| 256 | 256 | 12,312 | Dolev–Strong |

The first power of two at which any legal fault bound reaches Ladder is 131,072 with this choice of E. Early completion of an individual retrieval does not change the public branch selection. A tighter proved public bound could change this calculation.

## Measured model outputs

The synchronous simulator uses ideal signer identities and permanent omissions. It checks sender validity and agreement for both bits with a healthy network, silent peers and a silent origin. Rows separately record full-chain checks, distinct signer/bit pairs, modeled receive buffers and schedule deadlines. Serialization charges one byte per bit value and 66 bytes per signer entry. There is no packet loss among live peers. In the 128-party, t = 42 healthy run, the modeled exchange sent 16,256 unicast copies and 2,153,666 body bytes, with 32,385 naive signature checks. The scheduled decision is at round 43, or 430 ms if the round is 10 ms. These are logical counts and conditional times. Wall-clock crypto cost, headers, TCP and retransmission are outside the model.

Stem's GET path requests an identified point from peers and accepts an authenticated proposal returned by a holder. GET_HANDOFF and GET_REPLAY likewise retrieve existing certified material. In the modeled GET with 128 participants, one holder and a 64 KiB body, the request bodies total 6,096 bytes and the one response is 65,536 bytes. The receiver can get a copy after two message delays. If all 127 peers reply, the response bodies total 8,323,072 bytes. This calculation models payload recovery only. Existing proposal and finality checks remain necessary. Stake distribution affects Jolteon quorum and does not enter the paper's count-based selector. [Pinned Stem handler](https://github.com/a19q3/stem/blob/fefb8cc01771d59e228c72755ddd1242291f7f22/src/jolteon.rs#L1320-L1348)

**Decision.** Keep Stem recovery as implemented. The published construction selects the classical algorithm across the requested sweep, and none of the reviewed recovery handlers requires all peers to decide a fresh bit from one designated sender. A separate experiment could measure duplicate GET responses using an actual archive trace before tuning repair traffic.

## Replay

Run from the repository root using Python 3.10 or later.

```sh
python3 research/broadcast/compare.py > /tmp/stem-broadcast.json
xz -dc research/broadcast/results.json.xz | cmp - /tmp/stem-broadcast.json
```

## Native GET observations during a 12-block rejoin

A diagnostic build of the pinned reference recorded each payload GET routing decision and each holder's local lookup. The four-replica network stopped one node and confirmed 12 signed payments. The 16-replica network stopped two and confirmed 48. After a gap of at least 12 blocks, the nodes restarted and verified common finality. Only public point IDs and counters were archived.

| Replicas | Directed request decisions | Broadcast decisions | Holder hits | Scheduled proposal reply bytes | Requester/point groups with repeated replies |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 13 | 0 | 13 | 10,931 | 0 |
| 16 | 30 | 0 | 30 | 60,890 | 0 |

The two captures exercised the existing directed path. Neither gave an observed duplicate reply to eliminate. Scheduled reply bytes exclude TCP framing, retransmission and other protocol messages. These short, healthy-link restarts do not estimate the probability of broadcast fallback during prolonged outages. Retain the existing fallback.

Apply observe.patch to fefb8cc, build its release binary and run the following capture from this branch. Per-node temporary databases are removed after each archive write. The data file get-traces.json.xz includes every decision, reply size and verified localnet report.

```sh
python3 research/broadcast/capture_get.py --binary /path/to/instrumented-stem --lock /path/to/stem-lock --scratch /tmp/stem-get-db --output /tmp/stem-get-traces.json.xz
```
