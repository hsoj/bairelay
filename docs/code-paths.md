# bairelay — Code Paths

Mermaid companion to `docs/architecture.md`. Architecture explains *why* the
system is shaped this way; this file traces *what calls what*. Verified against
`main`, 2026-08-18 (single-crate tree, post `stream_translate.rs`). Line
numbers drift; module and function names are the durable anchors. Annotated
risks cite `docs/action-plan.md` item IDs.

---

## 1. Module map and layering

One crate. The four protocol directories know nothing about cameras; the
compiler no longer enforces this (the crates were merged) — review does.
Dotted edges are consumer-declared trait seams: the arrow always points from
camera-side into the protocol module, never back.

```mermaid
graph TD
    subgraph CAMSIDE ["camera side — src/*.rs"]
        MAIN["main.rs<br/><i>composition root, sync binds</i>"]
        ORCH["orchestrator.rs · supervisor.rs"]
        CH["camera.rs<br/><i>8 role traits + CameraHandle</i>"]
        BCC["bc_camera.rs<br/><i>only file naming BcCamera</i>"]
        MS["mqtt_status.rs<br/><i>only file naming StatusPublisher*</i>"]
        CP["camera_provider.rs"]
        SS["stream_source.rs<br/><i>fan-out driver</i>"]
        PURE["stream_translate.rs · gap_bridging.rs<br/><i>pure, no I/O</i>"]
    end

    BAI["src/baichuan/<br/><i>vendored BC protocol</i>"]
    RTSP["src/rtsp/<br/><i>RTSP/RTSPS server</i>"]
    MQTT["src/mqtt/<br/><i>broker bridge + HA discovery</i>"]
    WAKE["src/wake_server/<br/><i>local P2P cloud replacement</i>"]
    SYNC["src/sync.rs<br/><i>poison-recovery shim — shared kernel<br/>under all four directories</i>"]

    BCC -->|"implements the 8 role traits"| CH
    BCC --> BAI
    CP -.->|"implements StreamProvider<br/>declared in rtsp/provider.rs"| RTSP
    MS -.->|"implements StatusReporter<br/>declared in camera_status.rs"| MQTT
    MAIN --> ORCH --> CH
    CH --> SS --> PURE
    WAKE -->|"bcudp codec, 6 sites<br/>incl. pub error #from — S6-4"| BAI
    RTSP --> SYNC
    WAKE --> SYNC
    BAI --> SYNC

    EROSION["known erosion (S6-4):<br/>mqtt_loop.rs names StatusPublisher directly ·<br/>camera.rs names crate::mqtt for HA discovery ·<br/>mqtt re-exports rumqttc types matched camera-side"]
    EROSION -.-> MQTT

    classDef pure fill:#14532d,stroke:#052e16,color:#fff
    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class PURE,SYNC pure
    class EROSION warn
```

---

## 2. Startup sequence

`src/main.rs`. Every socket binds **synchronously before any "started" log
line**; a bind failure halts startup rather than half-starting the daemon.

```mermaid
flowchart TD
    START(["main()"]) --> MODE{"CLI subcommand<br/>src/cli.rs"}

    MODE -->|"snapshot / battery / ptz / …"| ONESHOT["run_support::run_oneshot"]
    MODE -->|"check-config"| CHECK["run_support::run_check_config_to"]
    MODE -->|"hassio"| HASS["hassio::cmd::run"]
    MODE -->|"mqtt-rtsp"| CFG["config::load_config<br/>read → parse → hydrate → validate"]

    CFG --> ORCH["Orchestrator + CameraHandles built"]
    ORCH --> MQTTLOOP["spawn run_mqtt_event_loop<br/><b>OUTSIDE the Supervisor</b>, own token"]
    MQTTLOOP --> SUP["Supervisor::new(token)"]

    SUP --> BINDS["synchronous binds — failure halts:<br/>RTSP TCP · RTSPS TCP · wake middleman UDP 9999 ·<br/>wake register UDP 58200 · push listener TCP"]
    BINDS --> SVCS["Supervisor::spawn each:<br/>watchdog 30 s · startup_wake · rtsp · rtsps ·<br/>wake_server · push_listener"]

    SVCS --> RUN["orchestrator.run()<br/><i>awaits every camera task, no budget</i>"]
    RUN --> DRAIN["cameras drain"]
    DRAIN --> SHUT["sup.shutdown(per-service timeout)"]
    SHUT --> FANOUT["publish_shutdown_fanout<br/>mqtt_loop.rs — final disconnected statuses"]
    FANOUT --> MCANCEL["mqtt_cancel.cancel()"]
    MCANCEL --> EXIT(["exit"])

    ONESHOT --> CODE["oneshot::classify → exit code"]
    CHECK --> CODE
    CODE --> EXIT
```

The MQTT event loop lives outside the `Supervisor` because per-camera teardown
publishes a final `disconnected` status through it — it must outlive every
camera task, and shutdown ordering (`sup.shutdown()` before
`mqtt_cancel.cancel()`) preserves that.

---

## 3. Cancellation token tree

Global → per-subsystem → per-camera → per-session → per-stream. Cancelling a
parent cancels every descendant; a session token kills pollers without tearing
down the camera.

```mermaid
graph TD
    G["global CancellationToken<br/>Ctrl+C handler"]
    MQ["MQTT event-loop token<br/><i>separate — outlives cameras</i>"]
    SUPT["Supervisor token"]

    G --> MQ
    G --> SUPT
    G --> CAM["per-camera token<br/>CameraHandle::run"]

    SUPT --> W["watchdog"]
    SUPT --> SW["startup_wake"]
    SUPT --> RS["RtspServer::serve_with_listener"]
    SUPT --> WS["wake_server::run_with_sockets"]
    SUPT --> PL["push_listener"]

    CAM --> SESS["session token<br/>spawn_session_tasks"]
    SESS --> ML["motion_listener"]
    SESS --> BP["battery_poller"]
    SESS --> PP["preview_poller"]
    SESS --> FP["floodlight_poller + listener"]
    SESS --> KA["keepalive_loop"]
    SESS --> SRC["StreamSource token"]
    SRC --> PACE["reader · translator · 3 pacer tasks"]
    RS --> CONN["per-connection token"] --> STASK["per-session task<br/>SessionEntry cancel"]

    ORPHAN["reachable by NO token (S5-10):<br/>main.rs per-MQTT-command spawn ·<br/>Ctrl+C handler task"]

    classDef tok fill:#7c2d12,stroke:#431407,color:#fff
    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class G,MQ,SUPT tok
    class ORPHAN warn
```

---

## 4. Per-camera lifecycle

`CameraHandle::run` — the state machine the whole battery design hangs off.

```mermaid
flowchart TD
    RUN(["CameraHandle::run"]) --> IDLE["wait_for_acquire<br/><i>camera stays asleep until a wake lock exists</i>"]
    IDLE --> CONNECT["bc_camera::connect<br/>discover → login, CONNECT_TIMEOUT-bounded"]

    CONNECT -->|"Ok"| SPAWN["spawn_session_tasks<br/>pollers + listeners + keepalive (session token)"]
    CONNECT -->|"ConnectError::Auth"| TERM(["STOP permanently<br/><i>never hammer bad credentials</i>"])
    CONNECT -->|"ConnectError::Other"| BACKOFF["ReconnectBackoff 2 s → 60 s cap"] --> IDLE

    SPAWN --> LIVE["run_connected_session<br/>keepalive_loop probes liveness"]

    LIVE -->|"last guard released"| GRACE["GracePeriod::run<br/><i>sleeps the window, checks idle at deadline —<br/>an acquire+release inside the window is invisible (S5-9)</i>"]
    GRACE -->|"idle at deadline"| TEAR["session token cancel →<br/>teardown_session_tasks<br/>final disconnected status via StatusReporter"]
    GRACE -->|"held at deadline"| LIVE
    LIVE -->|"keepalive fails / stream dies"| BACKOFF
    TEAR --> IDLE

    WD["watchdog — 30 s sweep<br/>fires only if idle_for ≥ grace,<br/>i.e. only when GracePeriod failed"] -.->|"safety net"| TEAR

    classDef term fill:#7f1d1d,stroke:#450a0a,color:#fff
    class TERM term
```

---

## 5. Media data path (camera → RTP)

The longest path in the system. Since S4-1 the translation core is **pure**:
`translate()` takes a packet, mutable state, a clock value, and the bridging
flag, and returns emits — no channel, lock, or socket in any signature.

```mermaid
flowchart LR
    subgraph CAM ["src/baichuan/"]
        direction TB
        C1["BcCamera::start_video"] --> C2["BcSubscription"] --> C3["BcMedia codec<br/>bcmedia/de.rs"]
    end

    subgraph DRV ["stream_source.rs — owns every channel/lock/buffer"]
        direction TB
        R["reader_task"] --> T["drive_translator_loop<br/><i>PacketSource seam</i>"]
        T --> AP["apply_bcmedia_packet<br/><i>fan-out driver</i>"]
        AP --> PACE["3 pacer tasks<br/>next_target() re-anchor policy<br/>queues: video 300 · audio 200, try_send-and-drop"]
        PACE --> BCST(["broadcast::Sender&lt;Frame&gt;"])
        AP --> LFB["LastFrameBuffer<br/>rtsp/buffer.rs"]
    end

    subgraph PURE ["pure layer — no I/O, time as parameter"]
        direction TB
        TR["stream_translate::translate<br/>→ (SmallVec&lt;Emit&gt;, Option&lt;pts&gt;)<br/>codec detect · NAL filter · PTS synthesis ·<br/>SDP derive · bridging audio gate"]
        GB["gap_bridging::BridgingPolicy<br/>Live ⇄ Bridging state machine"]
    end

    subgraph OUT ["src/rtsp/"]
        direction TB
        ST["session_task::run<br/>replay_burst() first — cached I-frame"]
        PK["packetizer.rs"] --> CD["codec/h264 · h265 · aac · g711<br/>transcode/adpcm"]
        CD --> TP["transport.rs<br/>TCP-interleaved or UDP<br/><i>writer mutex, no deadline — S5-1</i>"]
        ST --> PK
    end

    C3 --> R
    AP -->|"packet, state, now, bridging"| TR
    TR -->|"emits"| AP
    AP <-->|"tick / gap check"| GB
    BCST --> ST
    LFB --> ST
    TP --> CLIENT(["RTSP client"])

    classDef pure fill:#14532d,stroke:#052e16,color:#fff
    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class TR,GB pure
    class TP warn
```

`LastFrameBuffer` is the cross-cutting object: written by the driver on
I-frames, read by `replay_burst` so a joining client gets a keyframe instantly,
and pre-warmed at boot by `startup_wake::warm_last_frame_buffers`.

---

## 6. Gap bridging state machine

`src/gap_bridging.rs` — pure; `stream_source.rs::tick_bridging` is its clock
and driver. The battery concession: cameras stop sending, RTSP clients must
keep seeing continuous RTP.

```mermaid
stateDiagram-v2
    [*] --> Live : stream starts

    Live --> Bridging : no upstream packet for gap_threshold_secs<br/>(on_tick, driver supplies now)
    Bridging --> Live : upstream packet arrives<br/>(on_upstream_packet — PTS stays monotonic)

    state Live {
        [*] --> Forwarding
        Forwarding : real NALs broadcast
        Forwarding : audio forwarded on the wire
        Forwarding : I-frame NALs cached (lazy burst anchor)
    }

    state Bridging {
        [*] --> Replaying
        Replaying : re-broadcast cached I-frame NALs
        Replaying : synthesised PTS (advance_replay_pts)
        Replaying : audio DROPPED on the wire
        Replaying : audio PTS counters keep advancing
    }

    note right of Bridging
        Audio PTS advances while muted so
        A/V realigns the instant Live resumes.
    end note
```

---

## 7. RTSP request path

One task per TCP connection, one per PLAYing session. Auth is hardened:
Basic is offered/accepted only over TLS; digest verification is constant-time
across users.

```mermaid
sequenceDiagram
    autonumber
    participant C as RTSP client
    participant CN as connection.rs<br/>handle_connection
    participant A as protocol/auth.rs
    participant P as StreamProvider<br/>(camera_provider.rs)
    participant R as SessionRegistry
    participant ST as session_task.rs

    C->>CN: DESCRIBE rtsp://host/cam/main
    CN->>A: authenticate()
    A-->>CN: Digest challenge / Ok(user)<br/>(Basic only if is_tls)
    CN->>P: subscribe(camera, kind, user)
    Note over P: ACL check → wake lock acquire →<br/>SubscriptionHandle{rx, last_frame, sdp, guard}
    CN->>C: 200 + SDP

    C->>CN: SETUP (per track)
    CN->>R: create/extend session, allocate transport<br/>(udp_pool port pair or interleaved)
    CN->>C: 200 Transport … Session: {sid};timeout=30

    C->>CN: PLAY
    CN->>ST: spawn session task
    ST->>C: replay_burst() — cached I-frame first
    loop while playing
        ST->>C: RTP via video/audio dispatch loops
    end

    C->>CN: TEARDOWN
    CN->>R: drop session entry
    ST-->>P: SubscriptionHandle drop → wake lock released
    Note over R: 5 s sweep expires sessions idle > 30 s
```

---

## 8. Wake sources → wake lock

Every path that keeps a battery camera awake converges on
`WakeLockCounter` (`AtomicUsize` + two `Notify`s, permit-storing).

```mermaid
flowchart LR
    RTSPC(["RTSP client"]) -->|"DESCRIBE/SETUP →<br/>subscribe()"| CP["camera_provider.rs"]
    MQTTC(["MQTT wakeup command"]) -->|"dispatch_control"| MD["mqtt_dispatch.rs"]
    PUSH(["camera motion push (TCP)"]) --> PL["push_listener.rs<br/>hold for motion_wake_hold"]
    BOOT(["daemon start"]) --> SW["startup_wake.rs<br/>warm snapshot per camera"]

    CP -->|"acquire → RAII guard<br/>#[must_use]"| WL["WakeLockCounter<br/>0→1 notify_acquire ·<br/>1→0 notify_release"]
    MD -->|"acquire for command duration"| WL
    PL -->|"acquire + timed hold"| WL
    SW -->|"acquire during warm"| WL

    WL -->|"wait_for_acquire returns"| CH["CameraHandle::run<br/>connect + session"]
    WL -->|"release → deadline check"| GP["grace_period.rs"]
    GP -->|"idle at deadline"| CH

    classDef lock fill:#7c2d12,stroke:#431407,color:#fff
    class WL lock
```

Leak risk (S5-1): a session task wedged in `transport.rs::send_rtp` (no write
deadline) never drops its `SubscriptionHandle`, so the guard — and the camera —
never sleeps.

---

## 9. MQTT paths — control in, status out

```mermaid
flowchart TD
    BROKER([MQTT broker])

    BROKER -->|"poll()"| EL["run_mqtt_event_loop<br/>main.rs — own token"]
    EL --> CE["mqtt_loop::classify_event"]
    CE -->|"ConnAck<br/><i>code discarded — S5-4</i>"| CA["handle_connack:<br/>re-subscribe + republish discovery"]
    CE -->|"Publish"| PC["mqtt::parse_control_message<br/>ASCII allowlist on camera name"]
    CE -->|"error"| BK["MqttBackoff 1→30 s<br/><i>retries auth failures forever — S5-4</i>"]

    PC --> SPAWN["tokio::spawn per command<br/><i>no token, no cap — S5-10</i>"]
    SPAWN --> DC["mqtt_dispatch::dispatch_control<br/>wake lock for command duration ·<br/>CMD_TIMEOUT 30 s"]
    DC --> ROLES["role traits: Ptz · Lighting ·<br/>Power · Stills · DeviceAdmin"]
    ROLES --> BCAM["BcCamera via bc_camera.rs"]

    subgraph OUTB ["status out"]
        direction TB
        EV["CameraEvent<br/>(pollers · listeners · lifecycle)"]
        SR["StatusReporter port<br/>camera_status.rs"]
        MSR["MqttStatusReporter<br/>publish + cache atomically"]
        SP["StatusPublisher → topics"]
        EV --> SR --> MSR --> SP
    end

    DP["DiscoveryPublisher<br/><i>called from camera.rs directly,<br/>bypassing the port — S6-4</i>"] --> BROKER
    OV["preview_overlay<br/>caption on Connecting/Sleeping"] --> BROKER
    SP --> BROKER
    DC -->|"reply topic: OK / FAIL<br/>reflects actual outcome"| BROKER

    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class SPAWN,BK,DP warn
```

---

## 10. Camera connect and login (vendored core)

```mermaid
flowchart TD
    CONNECT["bc_camera::connect<br/><i>the adapter — classifies auth as terminal</i>"] --> NEW["BcCamera::new(opts)"]
    NEW --> FIND["find_camera"]

    FIND --> DM{"DiscoveryMethods"}
    DM -->|"None"| TCPONLY["TCP to known address"]
    DM -->|"Local"| BCAST["UDP broadcast on LAN<br/>connection/discovery.rs<br/><i>strategies raced, first answer wins</i>"]
    DM -->|"Remote"| REOL["Reolink servers for IP"]
    DM -->|"Cloud"| CLOUD["cloud.rs — sigV3 bundle<br/>(PoW solve on spawn_blocking)"]

    TCPONLY --> LOC["CameraLocation"]
    BCAST --> LOC
    REOL --> LOC
    CLOUD --> LOC

    LOC --> SRC{"transport"}
    SRC -->|"TCP"| TS["tcpsource.rs"]
    SRC -->|"UDP"| US["udpsource.rs<br/>BcUdp codec · CRC · reorder cap 1024"]
    TS --> BCONN["BcConnection<br/>AES-CFB · nom parsers"]
    US --> BCONN

    BCONN --> LOGIN{"login variant"}
    LOGIN -->|"legacy / modern"| L1["login.rs"]
    LOGIN -->|"cloud"| L3["sigV3 (lver=3)<br/><i>logs plaintext login body at debug — S5-3</i>"]
    L1 --> SESS["session: BcSubscription per msg id<br/>keepalive probe"]
    L3 --> SESS

    LOGIN -.->|"AuthFailed / CameraLoginFail"| TERM["ConnectError::Auth →<br/>reconnect loop breaks permanently"]

    classDef fail fill:#7f1d1d,stroke:#450a0a,color:#fff
    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class TERM fail
    class L3 warn
```

---

## 11. Wake server and push listener

The local replacement for Reolink's P2P cloud — a battery camera wakes without
traffic leaving the LAN.

```mermaid
flowchart LR
    CAMERA(["Reolink battery camera"])

    subgraph WS ["src/wake_server/"]
        direction TB
        MM["middleman.rs — UDP 9999<br/>C2M_Q / D2M_Q relay"]
        RG["register.rs — UDP 58200<br/>registration + heartbeat<br/><i>stale after 80 s</i>"]
        REG["registry.rs<br/>MAX_MAP_ENTRIES = 1024,<br/>refresh vs insert distinguished"]
        RT["route.rs — source-route cache<br/>CACHE_CAP 256 (process-global)"]
        PKT["packet.rs → baichuan::bcudp codec"]
        MM --> PKT
        RG --> PKT
        PKT --> REG --> RT
    end

    PL["push_listener.rs — TCP<br/>one task per conn, IP-gated<br/><i>uncapped — S5-10</i>"]

    CAMERA <--> MM
    CAMERA <--> RG
    CAMERA -->|"motion push"| PL
    RT -->|"10-packet wake burst<br/><i>2 spawns per request, no rate limit — S5-10</i>"| CAMERA
    PL -->|"motion event + wake hold"| WLK["wake lock + StatusReporter"]

    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class PL warn
```

---

## 12. One-shot command path

Connect, do one thing, log out, exit with a coarse code — fully separate from
the daemon.

```mermaid
flowchart TD
    CLI["cli.rs subcommand"] --> RO["run_support::run_oneshot"]
    RO --> RUN["oneshot::runner::run<br/>connect ≤ 100 s · login ≤ 30 s ·<br/>logout ≤ 5 s (always runs)"]
    RUN -->|"op(&dyn Camera)"| DISP["dispatch_oneshot"]
    DISP --> NARROW["narrow role per handler:<br/>&dyn Power · &dyn Ptz · &dyn Lighting ·<br/>&dyn DeviceAdmin · &dyn Stills"]
    NARROW --> OUT["oneshot/output.rs — text or --json"]
    RO -->|"cloud-authorise<br/>bypasses runner"| OUT
    OUT --> CLS["classify(err)"]
    CLS --> E["exit codes: 2 usage · 3 config ·<br/>4 connection/auth · 5 protocol ·<br/>6 unsupported · 130 Ctrl+C"]
```

---

## 13. Test seams

Everything is tested through consumer-declared traits, never live hardware.

```mermaid
graph LR
    subgraph PROD ["production impl"]
        direction TB
        P1["BcCamera (bc_camera.rs)"]
        P2["Discovery"]
        P3["BcStream"]
        P4["StreamDataSource"]
        P5["CameraProvider"]
        P6["rumqttc client"]
        P7["MqttStatusReporter"]
    end
    subgraph SEAM ["trait seam"]
        direction TB
        T1["camera::Camera —<br/>8 flat role traits"]
        T2["CameraDiscoverer"]
        T3["VideoStream"]
        T4["PacketSource"]
        T5["StreamProvider"]
        T6["SharedMqttClient"]
        T7["StatusReporter"]
    end
    subgraph TEST ["test double"]
        direction TB
        F1["FakeCameraBuilder + per-role fakes<br/>(FakePower, FakePtz, …) — cfg(test)"]
        F2["ScriptedDiscoverer"]
        F3["MockVideoStream"]
        F4["ScriptedSource / injected BcMedia"]
        F5["FakeStreamProvider"]
        F6["test_support::mock_client()<br/><i>gating gap — S5-6</i>"]
        F7["tested via mock_client capture"]
    end

    P1 --> T1 --> F1
    P2 --> T2 --> F2
    P3 --> T3 --> F3
    P4 --> T4 --> F4
    P5 --> T5 --> F5
    P6 --> T6 --> F6
    P7 --> T7 --> F7

    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class F6 warn
```

The pure modules (`stream_translate.rs`, `gap_bridging.rs`) need **no doubles
at all** — no runtime, no timeouts, microsecond tests. Prefer adding a policy
test there over driving the same logic through a task loop.

---

## 14. Shared-state map

Which objects cross task boundaries. The poison sweep landed: everything goes
through `src/sync.rs`'s recovery traits; zero locks are held across `.await`
(machine-enforced by `clippy::await_holding_lock` + `-D warnings`).

```mermaid
graph TD
    SYNC["src/sync.rs<br/>RwLockPoisonRecover · MutexPoisonRecover"]

    subgraph STATE ["cross-task state"]
        direction TB
        S1["CameraHandle — 7 small independent locks<br/><i>state + camera under TWO locks:<br/>split-brain readable — S6-1</i>"]
        S2["StreamSource internals<br/>translator state · SDP · buffers"]
        S3["SessionRegistry — every connection"]
        S4["LastFrameBuffer — every session"]
        S5["wake_server registries (capped)"]
        S6["StatusCache"]
    end

    S1 --> SYNC
    S2 --> SYNC
    S3 --> SYNC
    S4 --> SYNC
    S5 --> SYNC
    S6 --> SYNC

    ACT["message-passing instead of locks:<br/>pacer mpsc queues (bounded, reasoned) ·<br/>broadcast fan-out · watch for latest-value"]

    classDef good fill:#14532d,stroke:#052e16,color:#fff
    classDef warn fill:#78350f,stroke:#451a03,color:#fff
    class SYNC,ACT good
    class S1 warn
```

---

## Cross-reference to the action plan

Annotated risks in these diagrams map to `docs/action-plan.md` items:

| Diagram | Item |
|---|---|
| §5 §8 transport write, wake-lock leak | **S5-1** |
| §9 ConnAck discarded, infinite auth retry | **S5-4** |
| §3 §9 §11 token-less / uncapped spawns | **S5-10** |
| §10 sigV3 login body at debug | **S5-3** |
| §4 grace period check-at-deadline vs doc | **S5-9** |
| §13 mock_client gating gap | **S5-6** |
| §14 CameraHandle split-brain state | **S6-1** |
| §1 wake_server→baichuan edge, discovery bypassing the port | **S6-4** |
