# Writing lyrics for Suno

For the Style Prompt, musical direction, BPM, meter, casting, exclusions, slider choices, and examples, read `suno guide prompting`. This guide concerns words and structure.

## A practical workflow

1. Decide who sings, to whom, and why. For fiction, check the setting and vocabulary before writing. A song performed in-world should fit the performers and instruments available there.
2. Write a distinct central phrase or image. Let the verses advance the situation. Repeat a chorus when repetition serves the song; a lament, work song, comic verse, or chant need not use a pop structure.
3. Speak the lines at the intended pulse. Match stresses to important beats. Shorten an overloaded line or allow more musical space.
4. Save just the lyrics and section cues in a UTF-8 text file.
5. Use `suno prompt` to choose the sound, then preview and generate one pair. Audition before batching.

`suno write --genre indie-rock --theme 'a long journey home' --out song.txt` can provide an editable scaffold. Its genre defaults are examples. Replace every `<...>` placeholder and revise any preset direction that does not fit. `--project-out` holds the full planning document separately. Piped stdout is JSON; shell redirection is not a lyrics export.

## Lyrics file example

```text
[Verse 1]
The gate is shut, the road is white
We kept the lantern through the night

[Chorus]
Carry the light, carry it home
No one should walk this road alone

[Verse 2]
The first bird calls beyond the hill
We lift the latch; the house is still

[Chorus]
Carry the light, carry it home
No one should walk this road alone

[Outro]
Carry it home
```

Use this as a formatting example, not a required structure. The title, style brief, source notes, and instructions to the agent belong outside the lyric file.

## Prosody and performance

There is no universal six-to-ten-syllable or four-to-six-word rule. The same line can fit a slow ballad or become crowded at a fast tempo. Check stress, breath, phrase length, and the relationship between adjacent lines. Rhymes should preserve meaning and natural word order.

Plain section labels such as `[Verse]`, `[Chorus]`, `[Bridge]`, and `[Outro]` can help guide form. A concise cue such as `[Verse: dry spoken delivery]` is an experiment, not a guarantee that the model will obey it. Avoid a stack of contradictory performance tags.

Commas, line breaks, ellipses, and spelled-out vowels may influence phrasing or pronunciation. They do not set exact rest lengths or note durations. For an unusual name, test a phonetic spelling in one pilot before changing it throughout a project.

A memorable hook can use a clear image, natural rhythm, a singable vowel, or a repeated phrase. There is no established universal hook length, repeat count, first-seven-seconds rule, or optimum BPM that guarantees virality. Keep the hook suited to the song.

## Voice vocabulary

Choose a small number of compatible directions:

- Register/cast: adult low alto, rough baritone, bright tenor, mixed unison group.
- Texture: clear, raspy, breathy, resonant, nasal, grainy.
- Action: clipped consonants, projected calls, deadpan talk-singing, sustained notes, controlled vibrato.
- Phrasing: behind the beat, strict pulse, short breaths, long legato phrases.
- Space: dry room, close vocal, distant group, short natural reverberation.

Describe audible behavior. “Sad” alone does not choose between quiet speech, full-voice lament, and theatrical belting.

## Render and recover

```sh
suno prompt --genre 'indie rock' --bpm 110 --meter 4/4 --beat-unit quarter \
  --voice 'adult warm tenor' --delivery 'clear consonants, steady full voice' \
  --instruments 'clean electric guitar, bass, live drums' \
  --title 'Carry the Light' --lyrics-file song.txt
```

Execute the returned preview argv before the generation argv. Use v6 or another model currently returned by `suno models`. Earlier generation models have been retired; an old command using v4.5-all is no longer a cheap-draft workflow.

`--download ./songs/` waits for completion and saves MP3s. `suno status <ids> --wait --download ./songs/` resumes existing work. `suno download <id> --format wav` requests a prepared WAV separately.

For text only, `suno lyrics --prompt '...'` asks Suno for a lyric draft. For a broad idea without supplied lyrics, `suno describe --prompt '...'` asks Suno to create both words and audio.
