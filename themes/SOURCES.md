# Bundled themes

VS Code color themes shipped next to the executable. Each folder is laid out like an
installed VS Code extension (`package.json` with `contributes.themes`), keeps the upstream
`LICENSE.txt`, and contains the theme files unmodified.

| Folder | Source | Version | License |
| --- | --- | --- | --- |
| `dracula-theme.theme-dracula` | Open VSX `dracula-theme/theme-dracula` | 2.25.1 | MIT |
| `Catppuccin.catppuccin-vsc` | Open VSX `Catppuccin/catppuccin-vsc` | 3.19.0 | MIT |
| `enkia.tokyo-night` | Open VSX `enkia/tokyo-night` | 1.1.2 | MIT |
| `arcticicestudio.nord-visual-studio-code` | Open VSX `arcticicestudio/nord-visual-studio-code` | 0.19.0 | MIT |
| `jdinhlife.gruvbox` | Open VSX `jdinhlife/gruvbox` | 1.29.1 | MIT |
| `vscode.theme-solarized` | `microsoft/vscode` `extensions/theme-solarized-{dark,light}` @ `71344722827a1808719df4940d89c55e0d9e52fc` | — | MIT (Microsoft; derived from Colorsublime-Themes, MIT — see `ThirdPartyNotices.txt`) |

`vscode.theme-solarized/package.json` is written for this project (upstream uses localized
labels); everything else is copied as published.

The default Dark+ and Light+ themes are compiled into the binary from `src/theme/default/`
(`microsoft/vscode` `extensions/theme-defaults/themes` @ the same commit, MIT).
