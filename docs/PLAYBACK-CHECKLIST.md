# Dolby Vision playback acceptance

Setup supplied by the user: **Apple TV 4K → Infuse → LG C1; NAS through SMB**.
Status: **pending actual playback observations**.

For each output, record in the private audit folder:

| Identity | Value |
| --- | --- |
| Case | Standard MEL / standard FEL / P7 hybrid / P8 hybrid |
| File and SHA-256 | Pending |
| Converter commit and job report | Pending |
| Apple TV model and tvOS version | Pending |
| Infuse version | Pending |
| LG firmware / picture mode | Pending |
| Apple TV video output and Match Dynamic Range settings | Pending |
| Infuse Profile 8 Dolby Vision playback setting | Pending |

Enable content matching for the test and record the initial settings. Apple's
[Match Dynamic Range documentation](https://support.apple.com/en-ca/102277)
explains output switching. Infuse also has a Profile 8-specific Dolby Vision
toggle under playback settings; verify it is on for DV testing, as described in
[Firecore's settings reference](https://support.firecore.com/hc/en-us/articles/360015608854-Settings-Overview).

Run the following checks and record a timestamp plus pass/fail/uncertain for each.

| Check | What to observe |
| --- | --- |
| Dolby Vision indication | Record the TV indication and Infuse's source information separately. A permanently forced Dolby output mode alone does not establish content-triggered switching. |
| Start and continuation | Play from the beginning and continuously for at least ten minutes; note stalls, decode artifacts or unexpected mode changes. |
| Highlights | Inspect a bright scene for clipping, flashing, or abrupt brightness changes. |
| Dark scenes | Inspect gradients, shadow detail, color casts and flicker. |
| Scene changes | Check hard cuts for delayed or early brightness transitions. |
| Seeking | Seek forward/backward near the beginning, middle and end; verify picture recovery and audio/subtitle timing. |
| Aspect ratio | Inspect letterbox edges and any known ratio changes. Mark not applicable if the title has no changes. |
| Tracks | Switch available audio/subtitle tracks; verify synchronization and expected forced subtitles. |

Compare questionable timestamps with the source and/or HDR10 base on the same
setup. For FEL sources, DV8 conversion discards FEL residual reconstruction;
do not require equivalence to a player reconstructing the original FEL picture.
Report unexpected changes rather than assuming every difference is intentional.

Send the case/file, software versions, timestamps and observations back for review.
Any failure or uncertain observation keeps playback acceptance pending until
investigated. Leave untested cases pending rather than copying another case's result.
