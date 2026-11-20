# Bounded UPnP protocol surface

`earph-network::upnp` supplies the U3 wire layer.
It does not enable a renderer in Engine or grant playback authority.
No existing owner, CLI, or desktop startup opens its listeners.
Core ownership and shared gain are qualified foundations.
U4 execution/events and U5 interoperability remain required gates.

The shared action table defines SOAP admission, ordered responses, and device/SCPD descriptions.
The surface declares MediaRenderer:1, AVTransport:1, RenderingControl:1, and ConnectionManager:1 under UDA1.0.
It omits SetNextAVTransportURI and all general administration operations.
It validates InstanceID=0, scalar types, Master channel, and Play speed=1 before queueing.
AVTransport and RenderingControl return their distinct invalid-instance faults, 718 and 702.
The nonseekable contract supplies Seek fault 710 and Next/Previous fault 711 after owner state validation.
Core supplies volume conversion, presets, and source ownership separately.
The wire descriptions and scalar validation share Core's volume limits.
The execution bridge and connection-state projection remain required.

`DeviceIdentity::new` validates the stable UDN, friendly name, and public SERVER header.
The caller supplies truthful OS/version, UPnP/1.0, and product/version metadata.
`HttpSurface::bind` requires one concrete IPv4 address and an explicit port.
It rejects wildcard, multicast, and broadcast bind addresses.
The planned product default is port 24680; port 0 requests an explicit ephemeral fixture listener.
The actual bound address is observable through `local_addr`.
Discovery stays absent until `attach_discovery` accepts one endpoint for that exact listener.

The surface owns one Tokio runtime thread.
Its HTTP connections, discovery polling, timers, and shutdown share that thread.
`try_recv` delivers a bounded `PendingAction` without executing it.
Each ticket has a process-local request ordinal, peer address, deadline, and cancellation observation.
It provides no cryptographic controller identity or Core authority.
The bridge must capture the real Core envelope once and retain it for internal replay.
It must check grants, generations, revision, ownership, and deadline inside actual Core admission.
Independent SOAP requests are not deduplicated by body hashes.
`respond` encodes the actual bridge result and refuses canceled, expired, or stopped tickets.
No successful mutation response is synthesized before that result.
Missing or expired bridge delivery returns Action Failed, without manufacturing owner state.

The payload reservation is 8 MiB for one surface.
A 256 KiB resident reservation covers descriptions, discovery queues, protocol metadata, and task bookkeeping.
Each live connection/ticket shares one reservation from the remaining budget.
At most four such reservations can coexist.
A retained canceled ticket keeps its reservation until the bridge drains or drops it.
Queued payloads share their reservation; cloned handles do not create another allowance.
The typed queues hold at most fourteen ordinary and two lifecycle tickets.
At most three ordinary requests wait for bridge results, preserving the fourth admission for Stop.
Four incomplete incoming headers can occupy ingress until their five-second deadlines.
These bounds do not promise that Stop bypasses incomplete socket framing.

HTTP uses one request per connection and disables keep-alive and pipelining.
Headers are capped at 32 KiB and 64 fields.
Duplicate Content-Length, conflicting framing, and unsupported transfer codings reject before typed dispatch.
Exactly one syntactically valid Host is required before admission.
Decoded chunked bodies share the 256 KiB SOAP limit.
Headers and bodies each have an absolute five-second read deadline.
Responses have an additional bounded connection lifetime and a 512 KiB encoded limit.
The encoder computes escaped byte counts before allocating the output.
The XML layer accepts UTF-8 XML 1.0, depth 16, 32 attributes per element, and 8192 elements.
It rejects DTDs, external/custom entities, malformed escapes, duplicate arguments, and duplicate expanded attribute names.
Decoded URI and metadata arguments remain bounded at 8192 and 65536 bytes.
Opaque metadata is escaped on output, without interpreting DIDL content as authority.
Raw SOAP values stay out of generic Debug output and static fault descriptions.

SSDP uses stable UDN targets and the selected HTTP LOCATION.
It generates alive, byebye, and M-SEARCH response messages for the six declared targets.
Datagrams are capped at 2 KiB, with 32 scheduled messages and ten sends per rolling second.
The same rate bound covers advertisements and search replies.
MX is validated and clamped to five seconds, within UDA's permitted downward clamp.
Replies that cannot meet MX expire rather than arriving late.
Malformed searches are discarded without an error datagram.
Unusable reply addresses and port zero never enter the scheduler.
An individual UDP port refusal discards that reply while preserving HTTP and discovery.
Other socket and interface errors stop the shared runtime.
Shutdown reports thread joining separately from the service error kind.
`from_bound` accepts owned selected-address UDP sockets and sends no unsolicited multicast.
The Linux multicast constructor binds the standard group/port, joins only the selected address, and disables IP_MULTICAST_ALL.
Its outbound socket binds the selected address and sets that same multicast interface.
TTL is explicit and bounded 1..255; UDA's planned initial value is 4.
Acquisition failures do not trigger interface scans, wildcard binds, or another address.

Non-Linux multicast acquisition currently returns Unsupported.
Mac selected-interface acquisition remains required renderer work.
An IP-only multicast join does not prove that interface restriction.
The proposed U4 configuration API is `SelectedInterface { address: Ipv4Addr, index: NonZeroU32 }`.
A Mac constructor must bind both sockets with socket2's `bind_device_by_index_v4(Some(index))`.
It must also bind/join the matching selected IPv4 address and set the outbound multicast interface.
The operator must verify the address/index pair explicitly; no automatic scan or broad fallback is proposed.
Darwin's bound-interface option controls send routing; it does not independently establish receive isolation.
Do not use socket2's indexed IPv4 membership API as Mac interface proof.
Darwin reads its traditional address fields and can ignore the trailing interface index.
Use a verified address/index pair and address-specific membership, then test actual receive and send isolation.
See [Darwin's membership handler](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/netinet/in_mcast.c#L2128).
This API is a proposal, not an implemented Mac capability or a waived acceptance requirement.

GENA endpoints have no subscription authority in this unit and return unsupported status.
U4 must implement real subscription/callback ownership before enabling advertisement.
Linux loopback tests exercise raw fragmented, chunked, oversized, ambiguous, slow, and canceled requests.
They also exercise real UDP discovery against the actual HTTP description listener.
They do not advertise onto a real LAN or establish controller interoperability.
Actual Music Assistant/GUPnP, Mac discovery, events, source ownership, gain behavior, and output remain required later gates.

The [Linux wire receipt](evidence/network/upnp-wire-linux-20261120.json) binds the frozen source and actual check results.

Primary contracts are [UDA1.0](https://upnp.org/specs/arch/UPnP-arch-DeviceArchitecture-v1.0.pdf),
[AVTransport:1](https://upnp.org/specs/av/UPnP-av-AVTransport-v1-Service.pdf),
[RenderingControl:1](https://upnp.org/specs/av/UPnP-av-RenderingControl-v1-Service.pdf),
and [ConnectionManager:1](https://upnp.org/specs/av/UPnP-av-ConnectionManager-v1-Service.pdf).
