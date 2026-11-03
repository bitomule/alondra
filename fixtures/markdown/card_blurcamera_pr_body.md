Close auto-redact phase 1: label text boxes, cover text in the album and in Siri

The four design questions #142 left open, decided by the user on the phone:

(a) Covered automatically, as today. No change.

(b) A text box now says what it hides. `DetectedFace` carries an optional
`sensitiveTextKind`, set by `SensitiveTextDetection`, and the editor draws a
small label ("Bank account", "Card number", "Email address", "Phone number")
above the box. VoiceOver reads the same name, and Siri's face entity uses it
instead of calling an IBAN "Face 2".

(c) Always on, no switch. Measured on Alice's iPhone Air (iPhone18,4, iOS 27.0),
4032x3024 photos, median of 7 warm passes: the OCR pass costs 149-242 ms per
photo against 20-31 ms for face detection, so opening a photo goes from ~0.03 s
to ~0.18-0.28 s of detection. The first read after launch pays 328 ms for the
text model. `make text-bench IMAGES=...` reproduces it (`TextBenchRunner`).

(d) The editor never said "no faces found"; the two places that did were the
album and Siri's "Censor Faces in Photo", and neither looked for text at all:

- Album: every photo is now read for tier-1 text too. Text belongs to no person,
  so it is kept apart from the clusters and covered in every photo whatever the
  person selects. A selection with text and no faces goes straight to the
  results instead of ending on "no faces found" with the IBAN readable, and a
  photo opened from the results brings its text boxes into the editor, which
  before only got the faces and so never covered text in that path.
- Siri: covers text as well as faces, keeping text on its own blur (an emoji
  sized for a face leaves the ends of a card number readable), and only fails
  when there is neither.
- The remaining message now says "No faces or personal details found".

The Vision pass moved into `SensitiveTextReader` so the album, which reads each
photo inside an autoreleasepool and cannot await, shares it with the use case.

Ablation: removing the six fixes fails exactly the seven tests written for
them (1355 run, 13 skipped, 7 failures by name); restored, 0 failures.
Snapshots: only BulkViewSnapshotTests/test_errorBanner_lightMode re-recorded
(its title and message changed), iPhone 17 Pro 26.3, simpool slot-0, where the
other 143 baselines pass unchanged. Strings in en, es and de.
