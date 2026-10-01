---
name: suno
description: Write, generate, resume, and download Suno music with the suno CLI. Use for songs, lyrics, covers, remasters, stems, voice personas, and MP3/WAV/M4A/MP4 downloads. Discover syntax with suno agent-info --command and read suno guide prompting before composing.
---

# Suno

- Start with `suno agent-info --command prompt` and `suno guide prompting`. Read `suno guide songwriting` for lyric structure. Scope discovery to the command you need.
- `suno prompt` builds a musical brief offline: BPM, meter, beat unit, groove, voice, delivery, instruments, arrangement, and exclusions. It reports missing choices and returns generation/preview argv. Presets are examples; choose casting for the song.
- `suno write --out song.txt` creates editable lyric placeholders. Fill them before generating. `--out` is lyrics only; piped stdout is JSON.
- `suno generate --dry-run` validates offline. Use a stable `--request-id UUID` for paid submission and retries. A changed musical request needs a new UUID.
- Generate one pair, download, and audition voice, delivery, rhythm, words, and ending before a large batch. Do not claim an audio quality check from file metadata alone. Leave sliders unset unless intentionally choosing them; 8/92 is not a universal recipe.
- v6 is the default. Use `suno models` and `suno credits` for current account access. Standard v6 currently costs 10 credits per pair; Max Mode costs more.
- `--download DIR` implies waiting. Interrupted work resumes with `suno jobs`, then `suno status <ids> --wait --download DIR`. Never resubmit just because polling timed out. Unknown submissions need library inspection.
- Downloads support `--format mp3|wav|m4a|mp4` and `--source auto|studio|library`; `auto` selects the account-supported route. Successful clips include `local_path`.
- `--headless` permits invisible Chrome only. `--no-browser` permits HTTP only; captcha requirements are returned explicitly. Default mode can use an offscreen browser when invisible solving fails.
- Use `suno auth --cookie-stdin` or `--jwt-stdin` for supplied secrets. `--token` is a solved captcha, with `--token-provider 1|2`; it is not an auth JWT.
- Machine failures appear as one stderr JSON envelope. Its `details` may include clip IDs and recovery actions. Piped successes use stdout JSON; `agent-info` is a raw manifest.
- `--force` bypasses only the duplicate guard. Literal lyric placeholders use the separate `--allow-placeholders` override. Deletion uses `--confirm` (`--yes` remains an alias).
- Research/priming mode remains available through `suno write --mode priming`; read `suno guide priming` for its documented scope.

- On Windows, a signed-in regular browser may still fail cookie extraction. Use `suno auth --browser-login` and sign in to the separate window; credentials never belong in chat or logs.
- Covers accept UUIDs or Suno song URLs and inherit source lyrics/title/style. Override tags, lyrics, vocal direction, persona, ranges, and sliders deliberately. `--source-file source.json --dry-run` provides an offline preview.
- `reuse` creates fresh audio; `replace --start S --end E --lyrics-file FILE` changes an audio section. `add-vocals` and `add-instrumental` reference existing audio. Use current voice personas; legacy persona selection/creation is not implemented.
- `crop`, `cut`, `speed`, and `reverse` create derived edits. Resume crop/cut action IDs with `edit-status --wait`. Inspect the library after an uncertain edit/remaster submission; those operations lack request-ID receipts.
- `set --lyrics` changes displayed text only; it does not change the sung audio. Remaster uses the dedicated upsample endpoint.
- The fork does not expose uploads, persona creation/verification, mashups, custom model training, or Studio multitrack editing. Source evidence and local tests do not prove live account success. Use `scripts/verify-live.ps1` to collect that evidence, then audition the audio.
