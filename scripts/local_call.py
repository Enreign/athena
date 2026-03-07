#!/usr/bin/env python3
"""
local_call.py — Talk to Athena's telephony server without Twilio.

Pretends to be Twilio Media Streams: records your mic, sends mulaw audio to
Athena's /media-stream WebSocket, and plays the response through your speaker.

Usage:
    python3 scripts/local_call.py [--url ws://localhost:8089/media-stream] [--barge-in] [--mic-gain 2.0] [--mic-gate-hold-ms 200]

Requirements:
    - sox (brew install sox)
    - websockets (pip3.11 install websockets)
    - Athena telephony server running:
        cargo run --features telephony -- telephony

Flags:
    --barge-in   Enable mic during playback (use headphones to avoid echo feedback)
    --mic-gain   Multiply mic input before gating/sending (use >1.0 for quiet mics)
    --mic-gate-hold-ms   Keep the mic gate open briefly after it drops (reduces choppy audio)

Tips:
    - Run with RUST_LOG=athena::telephony=debug to see transcripts in server log.
    - Ctrl+C to hang up.
"""

import argparse
import asyncio
import audioop
import base64
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import uuid

try:
    import websockets
except ImportError:
    print("Missing: pip install websockets")
    sys.exit(1)

STREAM_SID = f"local-{uuid.uuid4().hex[:8]}"
CALL_SID = f"call-{uuid.uuid4().hex[:8]}"
CHUNK_BYTES = 160  # 20ms of mulaw audio at 8kHz
# Silence after response_end before re-opening mic, to let speaker tail off.
POST_SPEECH_DELAY = 0.8
PROCESS_START_GRACE = 0.25
DEBUG_CHUNK_LOG_INTERVAL = 50
DEFAULT_DEBUG_DUMP_DIR = "/tmp/athena-local-call"
MIC_ACTIVITY_RMS_THRESHOLD = 500
MIC_SILENCE_GATE_RMS = 40
MIC_SILENCE_GATE_PEAK = 120
MULAW_SILENCE_BYTE = 0xFF
DEFAULT_MIC_GAIN = 1.0
DEFAULT_MIC_GATE_HOLD_MS = 200


# ── sox helpers ──────────────────────────────────────────────────────────────

def start_recorder() -> subprocess.Popen:
    """Record from default mic at 8kHz mulaw, emit raw bytes to stdout."""
    return subprocess.Popen(
        ["sox", "-d", "-r", "8000", "-e", "mu-law", "-c", "1", "-t", "raw", "-"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def start_player() -> subprocess.Popen:
    """Play raw mulaw 8kHz bytes from stdin to default speaker."""
    return subprocess.Popen(
        ["sox", "-t", "raw", "-r", "8000", "-e", "mu-law", "-c", "1", "-", "-d"],
        stdin=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def read_process_stderr(proc: subprocess.Popen) -> str:
    if proc.stderr is None:
        return ""
    try:
        return proc.stderr.read().decode(errors="replace").strip()
    except Exception:
        return ""


async def ensure_audio_process(name: str, proc: subprocess.Popen) -> None:
    await asyncio.sleep(PROCESS_START_GRACE)
    if proc.poll() is None:
        return
    details = read_process_stderr(proc) or f"{name} exited with status {proc.returncode}"
    raise RuntimeError(f"{name} failed to start: {details}")


def dump_response_audio(dump_dir: str, response_index: int, mulaw_bytes: bytes) -> str | None:
    os.makedirs(dump_dir, exist_ok=True)
    stem = os.path.join(dump_dir, f"response-{response_index:03d}")
    mulaw_path = f"{stem}.mulaw"
    wav_path = f"{stem}.wav"
    meta_path = f"{stem}.json"

    with open(mulaw_path, "wb") as f:
        f.write(mulaw_bytes)

    meta = {
        "response_index": response_index,
        "bytes": len(mulaw_bytes),
        "seconds": round(len(mulaw_bytes) / 8000.0, 3),
        "created_at_epoch": time.time(),
        "mulaw_path": mulaw_path,
        "wav_path": wav_path,
    }
    with open(meta_path, "w", encoding="utf-8") as f:
        json.dump(meta, f, indent=2)

    result = subprocess.run(
        ["sox", "-t", "raw", "-r", "8000", "-e", "mu-law", "-c", "1", mulaw_path, wav_path],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        print(f"[debug] failed to convert {mulaw_path} to wav: {result.stderr.strip()}", flush=True)
        return None

    print(f"[debug] saved response audio to {mulaw_path} and {wav_path}", flush=True)
    return wav_path


def dump_mic_audio(dump_dir: str, mulaw_bytes: bytes) -> str | None:
    os.makedirs(dump_dir, exist_ok=True)
    mulaw_path = os.path.join(dump_dir, "mic-capture.mulaw")
    wav_path = os.path.join(dump_dir, "mic-capture.wav")
    meta_path = os.path.join(dump_dir, "mic-capture.json")

    with open(mulaw_path, "wb") as f:
        f.write(mulaw_bytes)

    meta = {
        "bytes": len(mulaw_bytes),
        "seconds": round(len(mulaw_bytes) / 8000.0, 3),
        "created_at_epoch": time.time(),
        "mulaw_path": mulaw_path,
        "wav_path": wav_path,
    }
    with open(meta_path, "w", encoding="utf-8") as f:
        json.dump(meta, f, indent=2)

    result = subprocess.run(
        ["sox", "-t", "raw", "-r", "8000", "-e", "mu-law", "-c", "1", mulaw_path, wav_path],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        print(f"[debug] failed to convert {mulaw_path} to wav: {result.stderr.strip()}", flush=True)
        return None

    print(f"[debug] saved mic audio to {mulaw_path} and {wav_path}", flush=True)
    return wav_path


async def play_wav_file(path: str) -> None:
    if shutil.which("afplay") is not None:
        cmd = ["afplay", path]
    else:
        cmd = ["sox", path, "-d"]
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdout=asyncio.subprocess.DEVNULL,
        stderr=asyncio.subprocess.PIPE,
    )
    _, stderr = await proc.communicate()
    if proc.returncode != 0:
        details = stderr.decode(errors="replace").strip() if stderr else ""
        raise RuntimeError(f"playback failed for {path}: {details or proc.returncode}")


def apply_gain_and_analyze(chunk: bytes, gain: float) -> tuple[bytes, int, int]:
    pcm16 = audioop.ulaw2lin(chunk, 2)
    if gain != 1.0:
        pcm16 = audioop.mul(pcm16, 2, gain)
    rms = audioop.rms(pcm16, 2)
    peak = audioop.max(pcm16, 2)
    if gain != 1.0:
        chunk = audioop.lin2ulaw(pcm16, 2)
    return chunk, rms, peak


# ── WebSocket tasks ──────────────────────────────────────────────────────────

async def send_audio(
    ws,
    recorder: subprocess.Popen,
    athena_speaking: asyncio.Event,
    barge_in: bool,
    dump_dir: str | None,
    mic_gate_enabled: bool,
    mic_gate_rms: int,
    mic_gate_peak: int,
    mic_gain: float,
    mic_gate_hold_ms: int,
) -> None:
    """Read mic chunks and forward them as Twilio media events.

    When barge_in is False (default), mic input is suppressed while Athena is
    speaking to prevent acoustic echo from triggering false barge-ins.
    """
    loop = asyncio.get_event_loop()
    sent_chunks = 0
    voiced_chunks = 0
    mic_capture = bytearray()
    max_rms = 0
    max_peak = 0
    gate_hold_frames = max(0, mic_gate_hold_ms // 20)
    gate_hold_remaining = 0
    try:
        while True:
            chunk = await loop.run_in_executor(None, recorder.stdout.read, CHUNK_BYTES)
            if not chunk:
                if recorder.poll() is not None:
                    details = read_process_stderr(recorder)
                    if details:
                        print(f"\n[mic] recorder exited: {details}", flush=True)
                break
            if not barge_in and athena_speaking.is_set():
                continue  # drop mic audio while Athena is speaking
            chunk, rms, peak = apply_gain_and_analyze(chunk, mic_gain)
            max_rms = max(max_rms, rms)
            max_peak = max(max_peak, peak)
            if rms >= MIC_ACTIVITY_RMS_THRESHOLD:
                voiced_chunks += 1
            if not mic_gate_enabled:
                gate_open = True
            else:
                gate_trigger = rms >= mic_gate_rms or peak >= mic_gate_peak
                if gate_trigger:
                    gate_hold_remaining = gate_hold_frames
                    gate_open = True
                elif gate_hold_remaining > 0:
                    gate_hold_remaining -= 1
                    gate_open = True
                else:
                    gate_open = False
            outgoing_chunk = chunk if gate_open else bytes([MULAW_SILENCE_BYTE]) * len(chunk)
            mic_capture.extend(outgoing_chunk)
            await ws.send(json.dumps({
                "event": "media",
                "streamSid": STREAM_SID,
                "media": {
                    "track": "inbound",
                    "chunk": "0",
                    "timestamp": "0",
                    "payload": base64.b64encode(outgoing_chunk).decode(),
                },
            }))
            sent_chunks += 1
            if sent_chunks == 1:
                print(
                    f"[mic] sending audio (rms={rms}, peak={peak}, gated={'no' if gate_open else 'yes'})",
                    flush=True,
                )
            elif sent_chunks % DEBUG_CHUNK_LOG_INTERVAL == 0:
                print(
                    f"[mic] sent {sent_chunks} chunks (rms={rms}, peak={peak}, active={voiced_chunks}, gated={'no' if gate_open else 'yes'})",
                    flush=True,
                )
    except Exception as e:
        print(f"\n[mic] stopped: {e}", flush=True)
    finally:
        if sent_chunks > 0:
            print(
                f"[mic] summary: chunks={sent_chunks}, active={voiced_chunks}, max_rms={max_rms}, max_peak={max_peak}",
                flush=True,
            )
            if voiced_chunks == 0:
                print(
                    "[warn] mic audio reached the server but looked near-silent; check input device and mic permissions",
                    flush=True,
                )
            elif max_rms < MIC_ACTIVITY_RMS_THRESHOLD and mic_gain <= 1.0:
                print(
                    "[hint] mic level looks low; try --mic-gain 2.0 or --no-mic-gate for local testing",
                    flush=True,
                )
        if dump_dir is not None and mic_capture:
            dump_mic_audio(dump_dir, bytes(mic_capture))


async def receive_audio(
    ws,
    player_ref: list | None,
    athena_speaking: asyncio.Event,
    dump_dir: str | None,
    playback_mode: str,
) -> None:
    """Receive Athena's audio responses and play them through the speaker."""
    loop = asyncio.get_event_loop()
    playback_ready_at = loop.time()
    response_audio_bytes = 0
    received_chunks = 0
    response_chunks = []
    response_index = 0
    try:
        async for raw in ws:
            event = json.loads(raw)
            kind = event.get("event")

            if kind == "media":
                athena_speaking.set()
                audio = base64.b64decode(event["media"]["payload"])
                response_audio_bytes += len(audio)
                received_chunks += 1
                response_chunks.append(audio)
                if received_chunks == 1:
                    print("[speaker] receiving audio", flush=True)
                elif received_chunks % DEBUG_CHUNK_LOG_INTERVAL == 0:
                    print(f"[speaker] received {received_chunks} chunks", flush=True)
                if playback_mode == "streaming":
                    playback_ready_at = max(playback_ready_at, loop.time()) + (len(audio) / 8000.0)
                    try:
                        player_ref[0].stdin.write(audio)
                        player_ref[0].stdin.flush()
                    except BrokenPipeError:
                        player_ref[0] = start_player()
                        await ensure_audio_process("speaker", player_ref[0])
                        player_ref[0].stdin.write(audio)
                        player_ref[0].stdin.flush()

            elif kind == "clear":
                # Barge-in: Athena wants to stop playback immediately.
                print("\n[barge-in]", flush=True)
                playback_ready_at = loop.time()
                response_audio_bytes = 0
                response_chunks.clear()
                athena_speaking.clear()
                if playback_mode == "streaming":
                    try:
                        player_ref[0].stdin.close()
                        player_ref[0].wait(timeout=0.5)
                    except Exception:
                        pass
                    player_ref[0] = start_player()
                    await ensure_audio_process("speaker", player_ref[0])

            elif kind == "mark":
                name = event.get("mark", {}).get("name", "")
                if name == "response_end":
                    if response_audio_bytes == 0:
                        print("[warn] server ended a response without sending audio", flush=True)
                    if response_audio_bytes > 0:
                        approx_secs = response_audio_bytes / 8000.0
                        response_index += 1
                        print(
                            f"[speaker] response complete: {response_audio_bytes} bytes (~{approx_secs:.1f}s)",
                            flush=True,
                        )
                        wav_path = None
                        if dump_dir is not None:
                            wav_path = dump_response_audio(dump_dir, response_index, b"".join(response_chunks))
                        if playback_mode == "buffered":
                            if wav_path is None:
                                print("[warn] buffered playback skipped because wav dump failed", flush=True)
                            else:
                                print(f"[speaker] playing buffered response {response_index}", flush=True)
                                await play_wav_file(wav_path)
                        else:
                            # Twilio returns a mark only after buffered playback finishes.
                            # Mirror that behavior locally instead of acknowledging it
                            # immediately, or the server re-opens too early while the
                            # speaker is still outputting Athena's audio.
                            wait_for_playback = max(0.0, playback_ready_at - loop.time())
                            if wait_for_playback > 0:
                                await asyncio.sleep(wait_for_playback)
                    await ws.send(json.dumps({
                        "event": "mark",
                        "streamSid": STREAM_SID,
                        "mark": {"name": "response_end"},
                    }))
                    await asyncio.sleep(POST_SPEECH_DELAY)
                    response_audio_bytes = 0
                    received_chunks = 0
                    response_chunks.clear()
                    athena_speaking.clear()
                    print("[listening...]", flush=True)

    except websockets.exceptions.ConnectionClosed:
        print("\n[server closed the connection]", flush=True)


# ── Main ─────────────────────────────────────────────────────────────────────

async def run(
    url: str,
    barge_in: bool,
    dump_dir: str | None,
    playback_mode: str,
    mic_gate_enabled: bool,
    mic_gate_rms: int,
    mic_gate_peak: int,
    mic_gain: float,
    mic_gate_hold_ms: int,
) -> None:
    print(f"Connecting to {url} ...")
    if dump_dir is not None:
        print(f"Debug dumps enabled: {dump_dir}")
    print(f"Playback mode: {playback_mode}")
    if mic_gate_enabled:
        print(
            f"Mic gate: enabled (rms<{mic_gate_rms} and peak<{mic_gate_peak} => silence, hold={mic_gate_hold_ms}ms)"
        )
    else:
        print("Mic gate: disabled")
    if mic_gain != 1.0:
        print(f"Mic gain: {mic_gain:.2f}x")

    async with websockets.connect(url) as ws:
        # Twilio handshake: connected -> start
        await ws.send(json.dumps({
            "event": "connected",
            "protocol": "Call",
            "version": "1.0.0",
        }))
        await ws.send(json.dumps({
            "event": "start",
            "streamSid": STREAM_SID,
            "start": {
                "streamSid": STREAM_SID,
                "callSid": CALL_SID,
                "tracks": ["inbound"],
                "mediaFormat": {
                    "encoding": "audio/x-mulaw",
                    "sampleRate": 8000,
                    "channels": 1,
                },
            },
        }))

        print(f"Call started  (stream={STREAM_SID})")
        if barge_in:
            print("Barge-in enabled — use headphones to avoid echo.\n")
        else:
            print("Mic muted while Athena speaks. Use --barge-in with headphones to interrupt.\n")

        # Start with Athena speaking (greeting plays immediately).
        athena_speaking = asyncio.Event()
        athena_speaking.set()

        recorder = start_recorder()
        player_ref = [start_player()] if playback_mode == "streaming" else None
        await ensure_audio_process("microphone", recorder)
        if playback_mode == "streaming":
            await ensure_audio_process("speaker", player_ref[0])

        try:
            await asyncio.gather(
                send_audio(
                    ws,
                    recorder,
                    athena_speaking,
                    barge_in,
                    dump_dir,
                    mic_gate_enabled,
                    mic_gate_rms,
                    mic_gate_peak,
                    mic_gain,
                    mic_gate_hold_ms,
                ),
                receive_audio(ws, player_ref, athena_speaking, dump_dir, playback_mode),
            )
        except asyncio.CancelledError:
            pass
        finally:
            recorder.terminate()
            recorder.wait()
            if playback_mode == "streaming":
                try:
                    player_ref[0].stdin.close()
                    player_ref[0].wait(timeout=1)
                except Exception:
                    pass
            try:
                await ws.send(json.dumps({"event": "stop", "streamSid": STREAM_SID}))
            except Exception:
                pass
            print("\nCall ended.")


def main() -> None:
    parser = argparse.ArgumentParser(description="Local Athena telephony client (no Twilio needed)")
    parser.add_argument(
        "--url",
        default="ws://localhost:8089/media-stream",
        help="Athena media-stream WebSocket URL",
    )
    parser.add_argument(
        "--barge-in",
        action="store_true",
        help="Keep mic open during playback (use headphones to avoid echo feedback)",
    )
    parser.add_argument(
        "--debug-dump-dir",
        default=DEFAULT_DEBUG_DUMP_DIR,
        help="Directory to save received response audio for inspection (use '' to disable)",
    )
    parser.add_argument(
        "--playback-mode",
        choices=["buffered", "streaming"],
        default="buffered",
        help="How to play Athena audio locally. Buffered is more reliable; streaming is closer to Twilio.",
    )
    parser.add_argument(
        "--no-mic-gate",
        action="store_true",
        help="Disable replacing quiet mic chunks with digital silence.",
    )
    parser.add_argument(
        "--mic-gate-rms",
        type=int,
        default=MIC_SILENCE_GATE_RMS,
        help="RMS threshold below which mic chunks may be replaced with silence.",
    )
    parser.add_argument(
        "--mic-gate-peak",
        type=int,
        default=MIC_SILENCE_GATE_PEAK,
        help="Peak threshold below which mic chunks may be replaced with silence.",
    )
    parser.add_argument(
        "--mic-gain",
        type=float,
        default=DEFAULT_MIC_GAIN,
        help="Multiply mic signal before gating/sending (use >1.0 for quiet mics).",
    )
    parser.add_argument(
        "--mic-gate-hold-ms",
        type=int,
        default=DEFAULT_MIC_GATE_HOLD_MS,
        help="Keep the mic gate open for this long after it drops (reduces choppy audio).",
    )
    args = parser.parse_args()

    loop = asyncio.new_event_loop()

    def _shutdown() -> None:
        for task in asyncio.all_tasks(loop):
            task.cancel()

    loop.add_signal_handler(signal.SIGINT, _shutdown)
    loop.add_signal_handler(signal.SIGTERM, _shutdown)

    try:
        dump_dir = args.debug_dump_dir or None
        if args.barge_in and args.playback_mode != "streaming":
            raise RuntimeError("--barge-in requires --playback-mode streaming")
        loop.run_until_complete(
            run(
                args.url,
                args.barge_in,
                dump_dir,
                args.playback_mode,
                not args.no_mic_gate,
                args.mic_gate_rms,
                args.mic_gate_peak,
                args.mic_gain,
                args.mic_gate_hold_ms,
            )
        )
    except asyncio.CancelledError:
        pass
    finally:
        loop.close()


if __name__ == "__main__":
    main()
