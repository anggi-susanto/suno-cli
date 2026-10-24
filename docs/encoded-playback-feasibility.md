# Historical encoded-playback investigation (retired)

This investigation and its experimental crypto addendum are superseded.
The unused local `media_crypto` experiment and `--source playback` download
route have been removed; neither encoded progressive playback downloads nor
local decryption is supported. Previous feasibility proposals and synthetic
crypto tests did not establish a supported provider contract.

Current download sources are `auto`, `studio`, `library`, and explicit
`remote-pool`. `auto` resolves to official signed Studio downloads when the
account is entitled, otherwise Library; MP4 always uses Library. Errors do not
trigger a fallback to metadata URLs or remote-pool. The `MediaUrl.encoding`
field is preserved solely as API metadata. See the README for current usage.
