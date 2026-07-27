# Bairelay — Code Paths

Mermaid companion to `docs/architecture.md`. Architecture explains *why* the
system is shaped this way; this file traces *what calls what*, with file and
line anchors so a diagram edge can be checked against source.

Anchors were verified against the tree at `2baf5a6`. Line numbers drift; module
and function names are the durable part.

---

## 1. Crate dependency graph

Four library crates plus one binary. The rule that keeps this acyclic: **no
library crate depends on another library crate except `wake-server → core`**,
and no library crate knows what a camera is except `core`.

```mermaid
graph TD
    BIN["bairelay<br/><i>bin + lib</i><br/>src/"]

    CORE["bairelay-neolink-core<br/><i>Baichuan protocol</i><br/>crates/core/"]
    RTSP["bairelay-rtsp<br/><i>RTSP/RTSPS server</i><br/>crates/rtsp/"]
    MQTT["bairelay-mqtt<br/><i>broker bridge + HA discovery</i><br/>crates/mqtt/"]
    WAKE["bairelay-wake-server<br/><i>local P2P replacement</i><br/>crates/wake-server/"]

    BIN --> CORE
    BIN --> RTSP
    BIN --> MQTT
    BIN --> WAKE
    WAKE --> CORE

    BIN -.->|"implements StreamProvider<br/>src/camera_provider.rs"| RTSP
    BIN -.->|"implements SharedMqttClient<br/>consumer, src/mqtt_loop.rs"| MQTT

    subgraph ext_core ["core third-party"]
        direction LR
        E1["aes · cfb-mode · x25519-dalek<br/>pbkdf2 · sha2 · md5 · zeroize"]
        E2["nom · cookie-factory · quick-xml<br/>crc32fast · reqwest"]
    end

    subgraph ext_rtsp ["rtsp third-party"]
        direction LR
        E3["rtsp-types · rtp-types · sdp-types"]
        E4["rustls · tokio-rustls"]
    end

    subgraph ext_mqtt ["mqtt third-party"]
        E5["rumqttc"]
    end

    CORE --> ext_core
    RTSP --> ext_rtsp
    MQTT --> ext_mqtt

    classDef bin fill:#1f4e79,stroke:#0d2b45,color:#fff
    classDef lib fill:#2d6a4f,stroke:#123528,color:#fff
    classDef ext fill:#5a5a5a,stroke:#2e2e2e,color:#fff
    class BIN bin
    class CORE,RTSP,MQTT,WAKE lib
    class E1,E2,E3,E4,E5 ext
```

The dotted edges are the point of the design, and they run the *same* direction
as the solid ones on purpose: there is no arrow from a library back to the
binary. `crates/rtsp/` declares `StreamProvider` (`crates/rtsp/src/provider.rs`)
and the binary supplies the impl (`src/camera_provider.rs`), so camera concepts
never enter the RTSP crate. Same for `crates/mqtt/`, which knows about topics
and payloads but not cameras.

**Known duplication** (`cargo tree -d`): `rand` 0.8 + 0.9, `thiserror` 1 + 2,
`getrandom` ×3, `rand_core` ×2, `hashbrown` ×2, `rustls-webpki` ×2, `syn` ×2.
`rand 0.8` is a *direct* dep of core, rtsp and wake-server — migrating those
three collapses most of it (remediation P2-4).

---

## 2. Startup sequence

`src/main.rs`. Every socket binds **synchronously before any "started" log
line**, so a bind failure halts startup rather than half-starting the daemon.

```mermaid
flowchart TD
    START(["main()<br/>src/main.rs:35"]) --> ASYNC["async_main()<br/>:48"]
    ASYNC --> MODE{"CLI subcommand<br/>src/cli.rs"}

    ONESHOT["run_support::run_oneshot<br/>src/run_support.rs:401"]
    CHECK["run_support::run_check_config_to<br/>:171"]
    HASS["hassio::cmd::run<br/>src/main.rs:82"]

    subgraph DAEMON ["daemon path"]
        direction TB
        CFG["load_validated_config<br/>run_support.rs:38"]
        ORCH["Orchestrator::with_bcmedia_dump_and_discovery<br/>src/main.rs:231"]
        MQTTLOOP["spawn run_mqtt_event_loop<br/>:330 — OUTSIDE the Supervisor"]
        SUP["Supervisor::new(token)<br/>:350"]

        CFG --> ORCH --> MQTTLOOP --> SUP
    end

    subgraph BINDS ["synchronous binds — failure halts startup"]
        direction TB
        B1["TcpListener::bind — RTSP :391"]
        B2["TcpListener::bind — RTSPS :429"]
        B3["UdpSocket::bind — middleman 9999 :487"]
        B4["UdpSocket::bind — register 58200 :490"]
        B5["TcpListener::bind — push listener :547"]
    end

    subgraph SVCS ["then Supervisor::spawn"]
        direction TB
        S1["watchdog :355"]
        S2["startup_wake :366"]
        S3["rtsp :402"]
        S4["rtsps :442"]
        S5["wake_server :496"]
        S6["push_listener :555"]
    end

    MODE -->|"one-shot<br/>snapshot / battery / ptz / …"| ONESHOT
    MODE -->|"check-config"| CHECK
    MODE -->|"hassio"| HASS
    MODE -->|"mqtt-rtsp"| CFG

    SUP --> B1
    B1 --> S3
    B2 --> S4
    B3 --> S5
    B4 --> S5
    B5 --> S6
    SUP --> S1
    SUP --> S2

    SUP -->|"then await"| RUN["orchestrator.run()<br/>src/orchestrator.rs:146"]
    RUN --> DRAIN["cameras drain"]
    DRAIN --> SHUT["sup.shutdown(per_service_timeout)<br/>src/supervisor.rs:87"]
    SHUT --> FINAL["publish_shutdown_fanout<br/>src/mqtt_loop.rs:64"]
    FINAL --> EXIT(["exit"])

    ONESHOT --> CODE["classify() → exit code<br/>src/oneshot/classify.rs:15"]
    CHECK --> CODE
    CODE --> EXIT
```

The MQTT event loop deliberately lives **outside** the `Supervisor`: per-camera
teardown publishes a final `disconnected` status through it, so it must outlive
every camera task.

---

## 3. Cancellation token tree

Three levels. Cancelling a parent cancels every descendant; a session token
cancels pollers and listeners without tearing down the camera.

```mermaid
graph TD
    G["global CancellationToken<br/>src/main.rs — Ctrl+C via<br/>run_support::spawn_ctrl_c_cancel:90"]
    MQTTTOK["MQTT event-loop token<br/>separate, outlives cameras"]
    SUPTOK["Supervisor token<br/>src/supervisor.rs:40"]

    subgraph CAMTOK ["per-camera token — src/camera.rs"]
        direction TB
        CR["CameraHandle::run()<br/>src/camera.rs:1213"]
    end

    subgraph SESS ["per-session token — cancelled on disconnect"]
        direction TB
        ML["camera_tasks::motion_listener:27"]
        BP["camera_tasks::battery_poller:212"]
        PP["camera_tasks::preview_poller:307"]
        FP["camera_tasks::floodlight_poller:397"]
        FL["camera_tasks::floodlight_listener:433"]
        KA["core keepalive<br/>crates/core/src/bc_protocol/keepalive.rs"]
    end

    G --> MQTTTOK
    G --> SUPTOK
    G --> CR

    SUPTOK --> W["watchdog<br/>src/watchdog.rs — 30 s safety net"]
    SUPTOK --> SW["startup_wake::warm_last_frame_buffers<br/>src/startup_wake.rs:58"]
    SUPTOK --> RS["RtspServer::serve_with_listener<br/>crates/rtsp/src/server/listener.rs:63"]
    SUPTOK --> WS["wake_server::run_with_sockets<br/>crates/wake-server/src/lib.rs:64"]
    SUPTOK --> PL["push_listener::run_with_listener<br/>src/push_listener.rs:92"]

    CR --> ML
    CR --> BP
    CR --> PP
    CR --> FP
    CR --> FL
    CR --> KA

    classDef tok fill:#7c2d12,stroke:#431407,color:#fff
    class G,MQTTTOK,SUPTOK tok
```

---

## 4. RTSP request path

`crates/rtsp/src/server/`. One task per TCP connection; one task per PLAYing
session.

```mermaid
sequenceDiagram
    autonumber
    participant C as RTSP client<br/>(ffmpeg / go2rtc / VLC)
    participant L as listener.rs:63<br/>serve_with_listener
    participant CN as connection.rs:79<br/>handle_connection
    participant A as rtsp/auth.rs
    participant R as server/registry.rs<br/>SessionRegistry
    participant P as StreamProvider<br/>(binary)
    participant ST as session_task.rs:281<br/>run

    C->>L: TCP accept
    L->>CN: spawn handle_connection
    Note over CN: read loop → try_consume_request<br/>drains ALL pipelined requests per read

    C->>CN: OPTIONS
    CN->>C: 200 Public: …

    C->>CN: DESCRIBE rtsp://host/cam/main
    CN->>CN: scheme_matches_transport(is_tls) :320
    CN->>A: authenticate() :570
    A-->>CN: Challenge / Ok(user) / Forbidden
    CN->>P: subscribe(camera, kind, user)
    P-->>CN: SubscriptionHandle{rx, last_frame, sdp, guard}
    CN->>C: 200 + SDP (sdp.rs)

    C->>CN: SETUP (per track)
    CN->>R: create/extend session, allocate transport
    R->>R: udp_pool.rs — port pair
    CN->>C: 200 Transport: … Session: …

    C->>CN: PLAY
    CN->>ST: spawn session task
    ST->>ST: replay_burst() — cached I-frame first
    loop while playing
        ST->>ST: video_dispatch_loop:369 / audio_dispatch_loop:445
        ST->>C: RTP (interleaved TCP or UDP)
    end

    C->>CN: TEARDOWN
    CN->>R: drop session
    ST-->>P: drop SessionGuard → wake lock released
```

**P0-2 lives at step 8**: `authenticate()` emits a `WWW-Authenticate: Basic`
challenge and accepts a `Basic` header *unconditionally*, even though
`ConnectionState::is_tls` is right there and already consulted at line 320.

The session task is a coordinator: after the PLAY gate it spawns two
independent per-kind dispatch loops, each owning its own
`broadcast::Receiver`. Periodic **RTCP Sender Reports are deliberately not
emitted** — the SR helpers in `crates/rtsp/src/server/rtcp.rs` exist for a
future SR-emitting context (`session_task.rs:1–8`).

---

## 5. Media data path (camera → RTP)

The longest path in the system, and the one `src/stream_source.rs` (5441 lines)
exists to serve.

```mermaid
flowchart LR
    subgraph CAM ["camera — crates/core/"]
        direction TB
        C1["BcCamera::start_video<br/>bc_protocol/stream.rs"]
        C2["BcSubscription<br/>connection/bcsub.rs"]
        C3["BcMedia codec<br/>bcmedia/de.rs"]
        C1 --> C2 --> C3
    end

    subgraph SRC ["src/stream_source.rs"]
        direction TB
        R["reader_task :1263"]
        T["drive_translator_loop :1432<br/>(PacketSource seam)"]
        AP["apply_bcmedia_packet :1686"]
        IF["handle_iframe :1734"]
        PF["handle_pframe :2018"]
        AA["handle_aac :2217"]
        PACE["media_pacer_task :1065<br/>video_pacer_task :1025<br/>audio_pacer_task :993"]
        BC(["broadcast::Sender&lt;Frame&gt;"])
        LFB["LastFrameBuffer<br/>crates/rtsp/src/buffer.rs"]

        R --> T --> AP
        AP --> IF
        AP --> PF
        AP --> AA
        IF --> PACE
        PF --> PACE
        AA --> PACE
        PACE --> BC
        IF --> LFB
    end

    subgraph OUT ["crates/rtsp/"]
        direction TB
        SUB["session_task::run :281"]
        PK["server/packetizer.rs"]
        CD["codec/h264.rs · h265.rs<br/>codec/aac.rs · g711.rs"]
        TR["transcode/adpcm.rs<br/>transcode/resample.rs"]
        RTP(["RTP out<br/>server/transport.rs<br/><i>no periodic RTCP SR</i>"])

        SUB --> PK --> CD --> RTP
        TR --> CD
    end

    C3 --> R
    BC --> SUB
    LFB --> SUB

    classDef gap fill:#78350f,stroke:#451a03,color:#fff
    class PACE gap
```

`LastFrameBuffer` is the cross-cutting object: written by `handle_iframe`, read
by `session_task::replay_burst` (`:491`) so a joining client gets a keyframe
immediately rather than waiting for the camera's next IDR. It is also what
`startup_wake::warm_last_frame_buffers` (`src/startup_wake.rs:58`) pre-populates
at boot via `capture_snapshot_into_buffer`.

`replay_burst` reuses the burst's `captured_pts_90khz` rather than restarting at
zero, and deliberately omits the in-band parameter-set replay — both are
go2rtc/ffmpeg interop workarounds documented at the call site.

---

## 6. Gap bridging state machine

`src/stream_source.rs:140–223`. The battery-camera concession: cameras stop
sending, but RTSP clients must keep seeing continuous RTP or they disconnect.

```mermaid
stateDiagram-v2
    [*] --> Live : stream starts<br/>(no spurious Bridging at startup)

    Live --> Bridging : no upstream packet for<br/>gap_threshold_secs<br/>check_gap_and_update_state:1150

    Bridging --> Live : upstream packet arrives<br/>PTS counter continues monotonically

    state Live {
        [*] --> Forwarding
        Forwarding : real NALs from apply_bcmedia_packet
        Forwarding : audio forwarded on the wire
        Forwarding : cache I-frame NALs → LastFrameBuffer
    }

    state Bridging {
        [*] --> Replaying
        Replaying : emit_replay_frame_if_bridging:1181
        Replaying : re-broadcast cached I-frame NALs
        Replaying : synthesised PTS, 200 ms detection ticker
        Replaying : audio DROPPED on the wire —
        Replaying : but PTS counters keep advancing
    }

    note right of Bridging
        Audio PTS advances while muted so
        A/V realigns the instant Live resumes.
    end note
```

---

## 7. Wake lock lifecycle

`src/wake_lock.rs` — `AtomicUsize` plus **two separate** `Notify`s, both using
`notify_one()` so a permit is stored for a late waiter.

```mermaid
sequenceDiagram
    autonumber
    participant CL as RTSP client
    participant CP as camera_provider.rs:60<br/>subscribe
    participant WL as wake_lock.rs<br/>WakeLockCounter
    participant GP as grace_period.rs:32<br/>run
    participant CH as camera.rs:1213<br/>CameraHandle::run

    CL->>CP: DESCRIBE / SETUP
    CP->>CP: ACL check (permitted_users)
    CP->>WL: acquire() :84 → WakeLockGuard
    Note over WL: count 0 → 1<br/>notify_acquire.notify_one()
    WL-->>CH: wait_for_acquire() :115 returns
    CH->>CH: connect + login → session token
    CP->>CH: stream_source(kind) :597
    CH-->>CP: Arc<StreamSource>
    CP-->>CL: SubscriptionHandle (guard moves into it)

    CL->>CP: TEARDOWN / disconnect
    Note over CP: SubscriptionHandle dropped<br/>→ WakeLockGuard::drop
    CP->>WL: count 1 → 0<br/>notify_release.notify_one()
    WL-->>GP: countdown starts

    alt new client before grace expiry
        CL->>WL: acquire()
        WL-->>GP: countdown reset
    else grace expires
        GP->>CH: "Grace period expired, disconnecting"<br/>src/camera.rs:1127
        CH->>CH: teardown session → "Disconnected" :1200
    end
```

`WakeLockGuard` (`src/wake_lock.rs:26`) carries **no `#[must_use]`** —
`wake_lock.acquire();` as a bare statement acquires and instantly releases
(remediation P2-3).

---

## 8. MQTT control path

```mermaid
flowchart TD
    BROKER([MQTT broker])

    BROKER -->|"subscribe"| EL["run_mqtt_event_loop<br/>src/main.rs:604"]
    EL --> CE["mqtt_loop::classify_event :119"]

    CE -->|"ConnAck"| CA["handle_connack :216<br/>re-subscribe, republish discovery"]
    CE -->|"Publish"| PC["mqtt::parse_control_message<br/>crates/mqtt/src/control.rs"]
    CE -->|"Disconnect / error"| RC["reconnect with backoff"]

    PC -->|"ASCII allowlist on camera name"| DC["mqtt_dispatch::dispatch_control<br/>src/mqtt_dispatch.rs:16"]

    DC --> CMD{ControlCommand}
    CMD --> C1["PtzMove / PtzPreset / PtzAssign"]
    CMD --> C2["Floodlight / Siren / StatusLight"]
    CMD --> C3["Pir / Reboot / SetTime"]
    CMD --> C4["Snapshot / Preview"]

    C1 --> DRV["Arc&lt;dyn CameraDriver&gt;<br/>crates/core/src/bc_protocol/camera_driver.rs:28"]
    C2 --> DRV
    C3 --> DRV
    C4 --> DRV

    DRV --> BCC["BcCamera<br/>crates/core/src/bc_protocol.rs"]

    subgraph PUB ["outbound"]
        direction TB
        SP["StatusPublisher<br/>crates/mqtt/src/status.rs"]
        DP["DiscoveryPublisher<br/>crates/mqtt/src/discovery/publisher.rs"]
        SC["StatusCache<br/>src/status_cache.rs"]
        OV["preview_overlay::rendered_preview<br/>src/preview_overlay.rs"]
    end

    BCC --> SC --> SP --> BROKER
    DP --> BROKER
    OV --> BROKER

    POLL["camera_tasks pollers<br/>battery :212 · preview :307<br/>floodlight :397 · motion :27"] --> SC
    POLL --> OV
    C4 --> OV

    classDef issue fill:#7f1d1d,stroke:#450a0a,color:#fff
    class DC issue
```

`preview_overlay` sits on the **MQTT** path only, not the RTSP one: it decodes
the preview JPEG and draws a `Connecting` / `Sleeping` caption before publish
(`Live` passes through untouched). Callers are `camera_tasks.rs:371` and
`mqtt_dispatch.rs:238`. The RTSP path's equivalent — showing something while the
camera sleeps — is gap bridging (§6), a different mechanism entirely.

`mqtt_dispatch.rs` is the one genuine layering inversion: at `:195` and `:343`
it manufactures `bairelay_neolink_core::bc_protocol::Error::Other(…)` for
failures that are purely binary-layer concerns ("PTZ preset name not in cache",
"Command timed out"). Remediation P3-2.

---

## 9. Camera connection path (core)

```mermaid
flowchart TD
    NEW["BcCamera::new(opts)<br/>crates/core/src/bc_protocol.rs:483"]
    NEW --> FIND["find_camera :274<br/>→ find_camera_with_discoverer :295"]

    FIND --> DM{"DiscoveryMethods<br/>bc_protocol/resolution.rs:47"}
    DM -->|None| TCPONLY["TCP to known addr only"]
    DM -->|Local| BCAST["UDP broadcast on LAN<br/>connection/discovery.rs"]
    DM -->|Remote| REOL["Reolink servers for IP<br/>then direct"]
    DM -->|Cloud| CLOUD["cloud.rs — account bound<br/>sigV3 (lver=3)"]

    TCPONLY --> LOC["CameraLocation"]
    BCAST --> LOC
    REOL --> LOC
    CLOUD --> LOC

    LOC --> CONN{"ConnectionProtocol"}
    CONN -->|Tcp| TS["tcpsource.rs"]
    CONN -->|Udp| US["udpsource.rs<br/>BcUdp codec + CRC"]
    CONN -->|TcpUdp| BOTH["try TCP, fall back UDP"]

    TS --> BCONN["BcConnection<br/>connection/bcconn.rs"]
    US --> BCONN
    BOTH --> BCONN

    BCONN --> CODEX["bc/codex.rs<br/>AES-CFB · nom parsers"]
    CODEX --> LOGIN{"login variant"}
    LOGIN --> L1["login.rs — legacy"]
    LOGIN --> L2["login_authlogin.rs"]
    LOGIN --> L3["login_sigv3.rs — cloud"]

    L1 --> SESS["logged-in session<br/>BcSubscription per message id"]
    L2 --> SESS
    L3 --> SESS

    SESS --> KA["keepalive.rs — periodic ping"]
    SESS --> OPS["stream · snap · battery · ptz<br/>motion · floodlight · pir · users · …"]

    classDef fail fill:#7f1d1d,stroke:#450a0a,color:#fff
    AUTHFAIL["auth failure →<br/>STOP retrying permanently"]:::fail
    LOGIN -.-> AUTHFAIL
```

Connection failures retry with backoff and never crash the process. **Auth
failures stop retrying permanently** — the daemon must not hammer a camera with
bad credentials.

---

## 10. Wake server and push listener

The local replacement for Reolink's P2P cloud, so a battery camera can be woken
without any traffic leaving the LAN.

```mermaid
flowchart LR
    CAMERA(["Reolink battery camera"])

    subgraph WS ["bairelay-wake-server"]
        direction TB
        MM["middleman.rs<br/>UDP :9999"]
        RG["register.rs<br/>UDP :58200"]
        REG["registry.rs<br/>CameraRegistry + SessionAnchors<br/>MAX_MAP_ENTRIES = 1024"]
        RT["route.rs<br/>cache, CACHE_CAP soft cap"]
        PKT["packet.rs → core bcudp codec"]

        MM --> PKT
        RG --> PKT
        PKT --> REG
        REG --> RT
    end

    subgraph PLS ["src/push_listener.rs"]
        PL["run_with_listener :92<br/>TCP, push notifications"]
    end

    CAMERA <-->|"C2M_Q / D2M_Q<br/>C2R_C / D2R_C"| MM
    CAMERA <-->|"registration + heartbeat"| RG
    CAMERA -->|"motion push"| PL

    RT -->|"wake burst"| CAMERA
    PL --> ORCH["Orchestrator / CameraHandle<br/>wake lock acquire"]

    classDef clean fill:#14532d,stroke:#052e16,color:#fff
    class REG,RT clean
```

Every in-memory map here is capped with refresh-vs-insert distinguished, so a
hostile flood cannot amplify memory — verified clean in the remediation review.

---

## 11. One-shot command path

Separate from the daemon entirely: connect, do one thing, log out, exit with a
coarse code.

```mermaid
flowchart TD
    CLI["Cli::Command<br/>src/cli.rs"] --> RO["run_oneshot_to<br/>src/run_support.rs:254"]
    RO --> FC["find_camera_config<br/>src/oneshot/dispatch.rs:56"]
    FC --> RUN["oneshot::runner::run<br/>src/oneshot/runner.rs:31"]

    subgraph RUNNER ["runner::run(cfg, cancel, op) — every step timeout-wrapped"]
        direction TB
        R1["BcCamera::new<br/>CONNECT_TIMEOUT 100 s"]
        R2["login_with_maxenc<br/>LOGIN_TIMEOUT 30 s"]
        R3["op(cam) — the injected closure"]
        R4["logout — LOGOUT_TIMEOUT 5 s<br/>runs regardless of op outcome"]
        R1 --> R2 --> R3 --> R4
    end

    RUN --> R1
    R3 -.->|"op is a closure over"| DISP["dispatch_oneshot(cam, cmd, json)<br/>src/oneshot/dispatch.rs:80<br/><i>passed in at run_support.rs:361</i>"]

    DISP --> OPS["snapshot · battery · reboot · ptz<br/>users · abilities · presets · pir<br/>siren · floodlight · status_light<br/>set_time · services · version"]

    CLOUD["cloud_authorise::run<br/>run_support.rs:293<br/><i>bypasses runner entirely</i>"]
    RO --> CLOUD

    R4 --> OUT["oneshot/output.rs<br/>text or --json"]
    OPS --> OUT
    CLOUD --> OUT
    OUT --> CLS["classify(err)<br/>src/oneshot/classify.rs:15"]

    CLS --> E2["2 — usage"]
    CLS --> E3["3 — config"]
    CLS --> E4["4 — connection / auth"]
    CLS --> E5["5 — protocol"]
    CLS --> E6["6 — unsupported"]
    CLS --> E130["130 — Ctrl+C"]
```

Splitting `dispatch_oneshot` out of `runner::run` is what lets the whole command
table be tested against `FakeCameraBuilder` without a TCP socket
(`src/oneshot/dispatch.rs:4`).

The nesting is also why this path shows up in remediation P2-5: the composed
future at `src/oneshot/runner.rs:50` measures ~34 KB. It is not the largest in
the workspace — that is `src/main.rs:101` at ~36 KB, with four more around
36 KB in `src/run_support.rs`.

---

## 12. Test seams

Everything is tested through traits rather than live hardware. Core's fakes sit
behind the `test-util` Cargo feature so a release build cannot substitute a fake
for a real camera.

```mermaid
graph LR
    subgraph PROD ["production impl"]
        direction TB
        P1["BcCamera"]
        P2["Discovery"]
        P3["BcStream"]
        P4["stream_source reader"]
        P5["CameraProvider"]
        P6["rumqttc client"]
    end

    subgraph TRAIT ["trait seam"]
        direction TB
        T1["CameraDriver<br/>core/bc_protocol/camera_driver.rs:28<br/><i>~42 methods</i>"]
        T2["CameraDiscoverer"]
        T3["VideoStream"]
        T4["PacketSource"]
        T5["StreamProvider<br/>rtsp/src/provider.rs"]
        T6["SharedMqttClient"]
    end

    subgraph TEST ["test impl"]
        direction TB
        F1["FakeCameraBuilder / FakeCalls<br/><i>test-util feature</i>"]
        F2["ScriptedDiscoverer"]
        F3["MockVideoStream"]
        F4["injected BcMedia"]
        F5["FakeStreamProvider"]
        F6["test_support::mock_client()<br/>→ MockHandle"]
    end

    P1 --> T1 --> F1
    P2 --> T2 --> F2
    P3 --> T3 --> F3
    P4 --> T4 --> F4
    P5 --> T5 --> F5
    P6 --> T6 --> F6

    classDef fat fill:#78350f,stroke:#451a03,color:#fff
    class T1 fat
```

`CameraDriver` is the one seam under active question: it mirrors `BcCamera` so
the forwarding blanket impl reads one line per method, but no consumer needs all
~42 and every fake pays for all of them. Remediation P3-3 asks for an explicit
decision either way.

**Hang-protection discipline**: every mock-based "camera doesn't answer" test
wraps the op in `tokio::time::timeout(Duration::from_millis(200), …)`. A test
awaiting a channel with no guaranteed sender hangs `cargo test` forever.

---

## 13. Shared-state map

Which objects cross task boundaries, and what guards them. This is the map that
makes remediation P0-3 (poison-panic cascade) legible.

```mermaid
graph TD
    subgraph BINSTATE ["binary — uses poison-recovering helpers"]
        direction TB
        B1["StreamTranslatorState<br/>src/stream_source.rs · 46 sites"]
        B2["CameraHandle fields<br/>src/camera.rs · 28 sites"]
        B3["StatusCache<br/>src/status_cache.rs · 10 sites"]
        HELP["lock_recover / rlock_recover / wlock_recover<br/>RwLockPoisonRecover / MutexPoisonRecover<br/>defined in stream_source.rs:88–135"]

        B1 --> HELP
        B2 --> HELP
        B3 --> HELP
    end

    subgraph LIBSTATE ["published libraries — panic on poison"]
        direction TB
        L1["SessionRegistry<br/>rtsp/server/registry.rs · 17 sites<br/><b>shared by EVERY connection</b>"]
        L2["LastFrameBuffer<br/>rtsp/src/buffer.rs · 6 sites<br/><b>shared by EVERY session</b>"]
        L3["CameraRegistry / SessionAnchors<br/>wake-server/registry.rs · 9 sites"]
        L4["session_task + udp_pool · 4 sites"]
    end

    subgraph OKG ["idiom already in-tree"]
        O1["wake-server/route.rs:77,83,96<br/>unwrap_or_else — p.into_inner"]
    end

    NOTE["expect(… poisoned) cascades one bug<br/>across every other holder — one panic<br/>takes down the whole server, not one client"]

    L1 -.-> NOTE
    L2 -.-> NOTE
    L3 -.-> NOTE
    L4 -.-> NOTE

    O1 -.->|"same fix, applied"| L3
    HELP -.->|"P3-1: move to src/sync.rs,<br/>then sweep the libraries"| L1

    classDef good fill:#14532d,stroke:#052e16,color:#fff
    classDef bad fill:#7f1d1d,stroke:#450a0a,color:#fff
    class HELP,O1 good
    class L1,L2,L3,L4,NOTE bad
```

The idiom already exists in-tree (`crates/wake-server/src/route.rs:77,83,96`) —
it is simply not applied uniformly. Note these sites use `.expect("… poisoned")`
rather than `.unwrap()`, so grepping for `unwrap` finds none of them.

---

## 14. Live-verify log-marker contract

`tests/scripts/manual-verify.sh` is the live-hardware gate and drives the daemon
purely by grepping stdout. Reword a marker and the script does not fail loudly —
it stalls its poll window and reports a *misleading* FAIL.

```mermaid
flowchart LR
    subgraph EMIT ["emitted by"]
        E1["crates/rtsp/src/server/listener.rs:78"]
        E2["src/main.rs:415"]
        E3["src/startup_wake.rs:102"]
        E4["src/camera.rs:1127"]
        E5["src/camera.rs:1200"]
    end

    subgraph MARK ["marker string"]
        M1["RTSP server listening"]
        M2["RTSP server started"]
        M3["Startup wake cycle complete"]
        M4["Grace period expired, disconnecting"]
        M5["Disconnected"]
    end

    subgraph USE ["manual-verify.sh stage"]
        U1["30 s startup gate"]
        U2["60 s warm-cycle gate"]
        U3["battery-sleep stage"]
    end

    E1 --> M1 --> U1
    E2 --> M2 --> U1
    E3 --> M3 --> U2
    E4 --> M4 --> U3
    E5 --> M5 --> U3

    PIN["src/log_capture.rs<br/>test-only tracing capture"]
    PIN ==>|"PINNED"| M3
    PIN -.->|"not yet pinned"| M1
    PIN -.->|"not yet pinned"| M4
    PIN -.->|"not yet pinned"| M5

    classDef pinned fill:#14532d,stroke:#052e16,color:#fff
    classDef loose fill:#78350f,stroke:#451a03,color:#fff
    class M3 pinned
    class M1,M4,M5 loose
```

---

## Cross-reference to the remediation plan

| Diagram | Remediation item |
|---|---|
| §4 RTSP request path, step 8 | P0-2 — Basic auth offered over plaintext |
| §13 Shared-state map | P0-3 — poison-panic cascade · P3-1 — move helpers to `src/sync.rs` |
| §1 Crate dependency graph | P1-1 — no supply-chain scanning · P2-4 — duplicate dep trees |
| §14 Log-marker contract | P1-4 — marker contract unpinned |
| §8 MQTT control path | P3-2 — error-type layering inversion |
| §11 One-shot path | P2-5 — large futures |
| §12 Test seams | P3-3 — `CameraDriver` is a ~42-method trait |
| §5 Media data path, §6 gap bridging | P3-6 — `stream_source.rs` at 5441 lines |
| §7 Wake lock lifecycle | P2-3 — `#[must_use]` missing on `WakeLockGuard` |
| §2 Startup sequence | P1-5 — `permitted_users` unvalidated by `check-config` |

Nothing in these diagrams depends on P0-1 as the remediation plan states it: the
discovery flood test passes and `cargo test` is green. The gate is red on
`cargo clippy --all-targets -- -D warnings` instead —
`src/log_capture.rs:112`, `await_marker` is never used.
