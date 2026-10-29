# Attribution

Every asset file in this repository, with its author, source and licence.

| File | Author | Source | Licence | Use |
|---|---|---|---|---|
| `assets/fonts/Inter-Regular.ttf` | The Inter Project Authors (Rasmus Andersson) | https://github.com/rsms/inter/releases/tag/v4.1 (`Inter-4.1.zip`, `extras/ttf/Inter-Regular.ttf`), via PlayCraft | OFL-1.1 | UI text (embedded by `ui-egui`) |
| `assets/fonts/Inter-OFL.txt` | The Inter Project Authors | same release, `LICENSE.txt` | OFL-1.1 | Inter's licence |
| `assets/fonts/JetBrainsMono-Regular.ttf` | The JetBrains Mono Project Authors | https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304 (`fonts/ttf/JetBrainsMono-Regular.ttf`), via PlayCraft | OFL-1.1 | Monospace text (embedded by `ui-egui`) |
| `assets/fonts/JetBrainsMono-OFL.txt` | The JetBrains Mono Project Authors | same release, `OFL.txt` | OFL-1.1 | JetBrains Mono's licence |

egui's built-in fonts are switched off. For Japanese, Chinese and Korean text the app also uses
a CJK font installed on the system (Noto Sans CJK and similar), loaded at run time and never
bundled.
