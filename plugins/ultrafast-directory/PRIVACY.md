# Privacy

This covers the ultrafast plugin for Claude and the open-source ultrafast
engine (`uf`) that it runs. Last updated 24 November 2026.

- The plugin is a skill: instructions and examples that Claude reads. It
  runs no server and no hooks, and it sends nothing anywhere.
- `uf` runs on your machine. It reads the documents and media you point it
  at and writes the files you ask for, in the folders you choose. It has no
  accounts, no analytics and no telemetry, and it sends nothing about you,
  your documents or your videos to ultrafast.
- `uf` goes online only to download what a render needs: media at an https
  URL that you put in a document or send to `uf serve` comes from that URL. A font family that
  a document names and that isn't installed comes from Google Fonts, and
  color emoji from Google's Noto Emoji release on GitHub. A `cargo install`
  build that can't find the Slang shader compiler downloads it from Slang's
  GitHub releases. Those requests go to Google and GitHub under their own
  privacy policies, and they name only the font or file wanted. Set
  `ULTRAFAST_OFFLINE=1` to turn font downloads off.
- What Claude does with your prompts and files is covered by Anthropic's
  privacy policy, not this one.
- Hosted ultrafast services, such as ultrafast Cloud, are not covered here
  and will have their own policy.

Questions: hello@ultrafast.mov.
