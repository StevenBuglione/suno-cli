# Directing Suno: sound, performance, and iteration

Checked September 28, 2026. Start with `suno prompt --help`; it builds an offline brief and exposes missing decisions. Its example presets are editable starting points, not tested optimum settings. Run `suno prompt --list-presets` to read them.

## What Suno documents

Suno's [v6 FAQ](https://help.suno.com/en/articles/13924481) describes richer understanding of musical direction. It says Variety changes style prompts; zero retains your supplied style. Max Mode spends more credits on a generation. The CLI sends your tags directly and exposes `--max-mode`; it does not claim a verified mapping for the website's Variety control.

Suno's [Creative Sliders guide](https://help.suno.com/en/articles/6141377) puts normal Weirdness at 50 and describes Style Influence as strength of adherence. Audio Influence applies to source audio. These are controls with different purposes. An 8/92 pair is not a universal quality setting; the CLI leaves unspecified sliders unset. Compare one intentional slider change after the musical direction is coherent.

Suno documents a separate [Exclude field](https://help.suno.com/en/articles/3161921) for unwanted instruments, styles, and other features. Use `--exclude`; keep desired sounds in `--tags`.

The [musical glossary](https://help.suno.com/en/articles/9010177) and [beat-making guide](https://suno.com/hub/step-by-step-guide-how-to-make-beats-with-suno) provide vocabulary for rhythm, genre, instruments, and tempo. Exact measured BPM, key, pronunciation, or instrumentation still needs checking in the result. A prompt is musical direction, not a score or a guarantee.

## Decide the sound before spending credits

Use specific choices for the dimensions that matter. You do not have to fill every field.

| Dimension | Useful direction | What a broad label leaves undecided |
| --- | --- | --- |
| Musical tradition | English village comic song; minimal house; modal lament | “Folk” can lead to several unrelated sounds |
| Emotion and energy | Wry, rowdy, unsentimental | “Warm” often invites softness |
| Voice | One adult rough baritone, narrow range; adult low alto | `--vocal female` does not specify age, range, or performance |
| Delivery | Deadpan talk-singing, clipped punchlines, projected calls | “Emotional” does not tell the singer what to do |
| Tempo and pulse | 104 dotted-quarter BPM, 6/8, two strong beats | 104 BPM alone leaves half/double-time ambiguity |
| Groove | Swung eighths, straight march, heavy downbeats | Tempo alone says nothing about accents |
| Instruments | Bowed fiddle answers; hand drum keeps pulse | A list gives less direction than instrumental roles |
| Arrangement | Solo verse, group response, abrupt last line | Every verse need not build to a pop chorus |
| Production | Dry small room, audible breath, narrow stereo | “Cinematic” may add an orchestra |
| Exclusions | Country twang, pedal steel, breathy pop vocals | A long unrelated blacklist can obscure priorities |

Casting is a creative decision. Do not infer the singer from the lyric narrator's gender, a character's age, or a generic genre preset. For a group or duet, describe the actual voices and their roles. Match a story song to its setting, available instruments, audience, and reason for being sung.

### Tempo, meter, and groove

- In 4/4, “quarter-note pulse at 100 BPM” usually states the intended counting unit clearly.
- In 6/8, two dotted-quarter beats per bar can give a jig or lilting compound feel. “88 dotted-quarter BPM” counts 88 of those main beats each minute; the eighth-note rate is 264 per minute. There are two main beats in each bar, each made of three eighth notes.
- In 3/4, three quarter-note beats per bar creates a different grouping from 6/8. Both contain six eighth notes, but their accents differ.
- Say half-time, double-time, swing, shuffle, or straight when that feel matters. “Fast” and “energetic” are not substitutes for pulse.
- A modal direction such as D Dorian is a target. Check the actual tonal center if it matters to the project.

### Voice and accompaniment are separate

“A cappella” means voices without instruments. `--instrumental` means no vocals. A solo song can have accompaniment; an unaccompanied song can have a choir. State each choice explicitly.

For a comic song, “gentle, warm, wistful, restrained” can fight the intended joke. Try concrete actions such as “dry punchlines, firm consonants, full voice, short pauses after each joke.” For an adult low voice, “youthful, light, airy” can fight the casting. These are prompt diagnoses, not claims about an audio file that nobody has heard.

## Field layout

- `--tags`: global sound and performance, in readable phrases or sentences.
- `--exclude`: a short list of unwanted sounds.
- `--lyrics-file`: words to sing, with section labels such as `[Verse]`, `[Chorus]`, and `[Bridge]`. Put any section-specific performance cue beside that section.
- `--title`: title only; never prepend it to the lyrics unless it should be sung.
- `--vocal`: optional male/female control; detailed casting remains in the style prompt. Use `--persona` to request an existing compatible voice persona.

Keep actual lyrics out of production prose. Section labels are useful cues; complex tags such as `[Energy: 73]` are not a documented deterministic language. Ordinary punctuation can guide phrasing, but it cannot enforce exact rests or note lengths.

## Original examples

These are starting briefs, not recordings with verified outcomes.

**Comic village song**

```sh
suno prompt --preset comic-folk --title 'The Missing Pie' --lyrics-file song.txt
```

The preset specifies an adult rough baritone, dry delivery, a jig pulse, fiddle responses, and hand drum. Override the cast with `--voice` to suit the song. The exclusions discourage a country ballad without suppressing fiddle itself.

**Unaccompanied work song**

```sh
suno prompt --preset work-song --title 'Pull Together' --lyrics-file song.txt
```

One adult caller and mixed adult unison response, 80 quarter-note BPM, heavy downbeats. The voice is doing physical work; avoid instructions for a polished cathedral choir unless that is intended.

**Plain solo lament**

```sh
suno prompt --preset solo-lament --voice 'one adult low tenor, chest register' \
  --title 'Empty Chair' --lyrics-file song.txt
```

A cappella, three stanzas, restrained vibrato, no large pop chorus. The explicit voice override replaces the preset's alto.

**Instrumental electronic track**

```sh
suno prompt --genre 'minimal house' --bpm 124 --meter 4/4 --beat-unit quarter \
  --groove 'four-on-the-floor, lightly swung hats' --instrumental \
  --instruments 'tight kick, sub bass, syncopated analog synth stabs' \
  --arrangement 'eight-bar intro, stripped break, final groove'
```

## Generate, audition, revise

1. Finish lyrics and brief. Inspect `style_prompt`, `negative_tags`, and `missing_directions` from `suno prompt`.
2. Run its `next_action.preview_argv` to validate the exact request offline. Execute argv as an argument array, without shell interpolation.
3. Run `next_action.argv` for one pair. Keep that request UUID with the song. Append `--download ./songs/` to save the audio.
4. Audition both results before producing an album or large batch. Check voice, delivery, pulse, arrangement, words, ending, and unwanted genre drift. State whether these were heard by a human, reviewed by an audio-capable system, or still unchecked. An MP3 decoder only verifies a playable file.
5. Keep the accepted clip IDs. Change one major cause of failure at a time. A revision needs a new request UUID; a retry of the same submission keeps the existing UUID.

Do not regenerate because a wait timed out. Run `suno jobs`, then `suno status <ids> --wait --download ./songs/`. If a submission outcome is unknown, inspect the library before sending another paid request.

Standard v6 generates two songs for 10 credits according to the current FAQ; account access and special inputs may change the cost. Read `suno models` and `suno credits` for current account state.
