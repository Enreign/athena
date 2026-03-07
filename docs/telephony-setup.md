# Telephony Setup Guide

Phone call support for Athena via Twilio Media Streams.

**Architecture**: Caller → Twilio → `POST /voice` (TwiML) → WebSocket `/media-stream` → mulaw 8kHz → Silero VAD → STT (Whisper) → LLM → TTS → audio back to caller.

```
┌──────────┐     ┌──────────┐     ┌──────────────────────────────────────┐
│  Caller  │────▶│  Twilio  │────▶│  Athena Telephony Server             │
│  (phone) │◀────│  (PSTN)  │◀────│                                      │
└──────────┘     └──────────┘     │  /voice ─▶ TwiML (connect stream)   │
                                  │  /media-stream ─▶ WebSocket          │
                                  │    ├─ Silero VAD (speech detection)  │
                                  │    ├─ Whisper STT (transcription)    │
                                  │    ├─ LLM (response generation)      │
                                  │    └─ TTS (speech synthesis)         │
                                  └──────────────────────────────────────┘
```

> **Local testing without Twilio:** Steps 1–3 and 7 are sufficient to run and
> talk to Athena locally using `scripts/local_call.py`. Steps 4–6 are only
> needed if you want real phone calls via a Twilio number.

---

## Prerequisites

| Component | Purpose | Required for |
|-----------|---------|--------------|
| STT server | Speech-to-Text | Local + Twilio |
| TTS server | Text-to-Speech | Local + Twilio |
| Silero VAD model | Voice Activity Detection | Local + Twilio |
| Twilio account | Phone number + Media Streams | Twilio only |
| Public URL (ngrok) | Twilio webhook reachability | Twilio only |

---

## Step 1: Download the Silero VAD Model

```bash
mkdir -p ~/.athena
curl -L -o ~/.athena/silero_vad.onnx \
  https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx
```

Verify: `ls -lh ~/.athena/silero_vad.onnx` — should be ~2MB.

## Step 2: Start an STT Server

Pick one:

**Option A — faster-whisper-server (recommended, self-hosted, free)**

Requires Python ≥ 3.10. On macOS with system Python 3.9, install Python 3.11 first:

```bash
brew install python@3.11
pip3.11 install faster-whisper-server

# Workaround for a pip-install bug: the package expects pyproject.toml in site-packages
echo '[project]
name = "faster-whisper-server"
version = "0.0.2"' > "$(pip3.11 show faster-whisper-server | grep Location | cut -d' ' -f2)/pyproject.toml"

# Model is a positional argument (not --model)
faster-whisper-server Systran/faster-whisper-large-v3 --port 8787
```

**Option B — whisper.cpp (self-hosted, free)**

See [whisper.cpp](https://github.com/ggerganov/whisper.cpp) for build instructions, then:
```bash
./server -m models/ggml-large-v3.bin --port 8787
```

**Option C — Groq (cloud, free tier)**

No server needed — just set the URL and API key in config:
```toml
stt_url = "https://api.groq.com/openai/v1/audio/transcriptions"
stt_api_key = "gsk_..."   # or ATHENA_STT_API_KEY env var
```

## Step 3: Start a TTS Server

Pick one:

**Option A — Kokoro TTS via Docker (self-hosted, free)**

[remsky/kokoro-fastapi](https://github.com/remsky/kokoro-fastapi) provides an OpenAI-compatible `/v1/audio/speech` endpoint:

```bash
docker run -d -p 8880:8880 ghcr.io/remsky/kokoro-fastapi-cpu:v0.2.2
```

Then set in config:
```toml
tts_url   = "http://localhost:8880/v1/audio/speech"
tts_model = "kokoro"
tts_voice = "af_heart"
```

> **Note:** `piper-tts` and `kokoro` (pip packages) are CLI synthesis tools only —
> neither has a built-in HTTP server mode. An OpenAI-compatible HTTP server is required.

**Option B — OpenAI TTS (cloud, $0.015/1K chars)**
```toml
tts_url = "https://api.openai.com/v1/audio/speech"
tts_api_key = "sk-..."   # or ATHENA_TTS_API_KEY env var
```

## Step 4: Configure Athena

Add to your `~/.athena/config.toml`:

```toml
[telephony]
listen_host = "0.0.0.0"
listen_port = 8089

# STT (adjust to match your choice from Step 2)
stt_url   = "http://localhost:8787/v1/audio/transcriptions"
stt_model = "whisper-large-v3"

# TTS (adjust to match your choice from Step 3)
tts_url   = "http://localhost:8880/v1/audio/speech"
tts_model = "kokoro"
tts_voice = "af_heart"

# Optional tuning
greeting       = "Hello, this is Athena. How can I help you?"
vad_silence_ms = 800    # ms of silence before end-of-speech (lower = faster, more false positives)
vad_threshold  = 0.5    # speech probability threshold (higher = stricter)
vad_enabled    = true   # set false to use energy-only detection (no Silero VAD)
stt_language   = "en"   # optional hint (reduces language mis-detect, speeds up tiny/base models)

# Required only for Twilio integration (Steps 5-7):
# twilio_account_sid = "ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
# twilio_auth_token  = "your_auth_token_here"
# public_url         = "https://xxxx.ngrok.io"
```

Credentials can also be set via environment variables:
```bash
export ATHENA_TWILIO_ACCOUNT_SID="AC..."
export ATHENA_TWILIO_AUTH_TOKEN="..."
export ATHENA_STT_API_KEY="..."        # only if using cloud STT
export ATHENA_TTS_API_KEY="..."        # only if using cloud TTS
```

## Step 5: Build and Run

```bash
cargo run --features telephony -- telephony
```

You should see:
```
Athena Telephony Server
  Listening on 0.0.0.0:8089
  Voice webhook: /voice
  Media stream:  /media-stream
```

---

## Local Testing Without Twilio

The `/media-stream` WebSocket endpoint has no authentication. You can connect to it
directly, bypassing Twilio entirely, using the included simulator script.

**Requirements:** `sox` (`brew install sox`) and `websockets` (`pip3.11 install websockets`).

```bash
# Terminal 1 — server with transcript logging:
RUST_LOG=athena::telephony=debug cargo run --features telephony -- telephony

# Terminal 2 — start a local call:
python3.11 scripts/local_call.py
```

The script:
- Sends a Twilio-format `connected` → `start` handshake over WebSocket
- Streams your mic audio as mulaw 8kHz media events (identical to what Twilio sends)
- Plays Athena's audio responses through your speaker via `sox`
- Handles barge-in (`clear` events) and end-of-turn (`mark` events)

Transcripts and LLM responses appear in the server log (`Transcript: "..."`).

Optional flags:
```bash
python3.11 scripts/local_call.py --url ws://localhost:8089/media-stream
python3.11 scripts/local_call.py --mic-gain 2.0 --mic-gate-hold-ms 400
python3.11 scripts/local_call.py --barge-in
```

---

## Twilio Integration (Steps 6–8)

Skip these steps if you only need local testing.

## Step 6: Expose Athena with a Public URL

Twilio needs to reach your server. Use ngrok for local development:

```bash
brew install ngrok/ngrok/ngrok

# ngrok requires a free account — sign up at https://dashboard.ngrok.com/signup
# then add your authtoken (one-time setup):
ngrok config add-authtoken <your-authtoken>

ngrok http 8089
# Note the https://xxxx.ngrok.io URL
```

Then add to your config:
```toml
public_url = "https://xxxx.ngrok.io"
```

## Step 7: Configure Twilio

1. Go to **Twilio Console** → **Phone Numbers** → **Manage** → select your number
2. Under **Voice Configuration**:
   - Set **"A call comes in"** webhook to: `https://xxxx.ngrok.io/voice`
   - Method: **HTTP POST**
3. Save

## Step 8: Test with a Real Call

Call your Twilio phone number. You should hear the greeting, then be able to have a conversation.

---

## Security

### Webhook Authentication

When `twilio_auth_token` is configured (recommended), Athena validates the
`X-Twilio-Signature` HMAC-SHA1 header on every `/voice` request. Unauthenticated
requests receive a `403 Forbidden` response.

If the token is omitted, signature validation is skipped (useful for local development).

Note: `/media-stream` (WebSocket) has no authentication — it relies on the `/voice`
TwiML handshake to gate access in production.

### Payload Limits

- Media payloads exceeding 32KB are dropped (Twilio normally sends ~214 bytes per 20ms chunk)
- LLM response accumulation is capped at 2,000 characters
- Audio buffer is capped at 60 seconds (480,000 samples)

---

## Barge-in (Interrupt)

Users can interrupt Athena mid-response by speaking. When speech is detected during TTS playback:

1. TTS chunk sending stops immediately
2. A Twilio `clear` event flushes queued audio
3. The user's new utterance is captured and processed normally

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| No answer / timeout | Twilio can't reach `/voice` | Check ngrok is running, URL matches Twilio config |
| `403 Forbidden` in Twilio logs | Signature mismatch | Verify `twilio_auth_token` matches Twilio Console, check `public_url` matches exactly |
| Greeting plays but no response | STT server not running or unreachable | Check `stt_url` is accessible: `curl http://localhost:8787/health` |
| Garbled / silent response | TTS server not running | Check `tts_url` is accessible: `curl http://localhost:8880/health` |
| "I couldn't hear that clearly" | STT transcription failed | Check STT server logs, ensure audio is reaching the server |
| Wrong language / gibberish transcript | STT language auto-detected incorrectly | Set `stt_language = "en"` (or your language) in config |
| Very slow response | VAD model not found, using energy fallback | Download Silero VAD model (Step 1) |
| Cuts off mid-word | `vad_silence_ms` too low | Increase to 800-1000ms |
| Long pauses before response | `vad_silence_ms` too high | Decrease to 500-600ms |
| `local_call.py`: no audio in | sox can't open mic | Run `sox -d -t raw - \| xxd \| head` to test mic access |
| `local_call.py`: no audio out | sox can't open speaker | Run `echo test \| sox -t raw -r 8000 -e mu-law -c 1 - -d` to test |

### Logs

Run with debug logging for full audio pipeline visibility:

```bash
RUST_LOG=athena::telephony=debug cargo run --features telephony -- telephony
```

You should see per-utterance timings like:
```
Utterance timings: queue=0ms, stt=620ms, llm=5400ms, tts_synth=2100ms, tts_encode=12ms, tts_send=4ms, total=8136ms
```

---

## PR Testing Checklist

Quick validation for reviewing telephony PRs:

### Build & Unit Tests
```bash
# Build with telephony feature
cargo build --features telephony

# Run all telephony unit tests
cargo test --features telephony telephony::

# Dead code gate
python3 scripts/dead_code_check.py --telephony

# Wiring checks
python3 scripts/wiring_check.py
```

### Local Smoke Test (no Twilio needed)

```bash
# 1. Start Athena telephony server
cargo run --features telephony -- telephony

# 2. Verify health endpoint
curl http://localhost:8089/health
# Expected: "ok"

# 3. Verify /voice returns TwiML (no auth token = no signature check)
curl -X POST http://localhost:8089/voice
# Expected: XML with <Response><Connect><Stream url="..."/></Connect>...</Response>

# 4. Verify /voice rejects bad signature (when auth token is set)
# Set twilio_auth_token in config, then:
curl -X POST http://localhost:8089/voice
# Expected: "Forbidden" (403) — no X-Twilio-Signature header

# 5. Full voice conversation test (no Twilio needed)
python3.11 scripts/local_call.py
# Speak into mic — verify greeting plays, speech is transcribed, response is spoken
```

### End-to-End Test (requires Twilio)

1. Configure Twilio + ngrok + STT + TTS (Steps 4-7 above)
2. Call the Twilio number
3. Verify:
   - [ ] Greeting plays on answer
   - [ ] Speech is transcribed (check debug logs for `Transcript: "..."`)
   - [ ] LLM response is spoken back
   - [ ] Barge-in works (speak during response — it should stop and listen)
   - [ ] Long silence after speaking triggers end-of-speech correctly
   - [ ] Call disconnect is handled cleanly (no errors in logs)
   - [ ] Multiple back-and-forth turns work in a single call
