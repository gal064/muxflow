## Android connection and update fix

Restores Android SSH connections and update checks by preserving the crypto
classes the release build loads at runtime. The release build now checks
for those classes in the signed APK.

If you installed Android 0.1.37, download the 0.1.38 APK below, open it on
your phone, and tap **Update**. Install it over 0.1.37 to keep your settings
and SSH identity. The 0.1.37 in-app updater is affected by the same bug.

## Install

```sh
curl -fsSL https://github.com/gal064/muxflow/releases/download/{{TAG}}/install.sh | MUXFLOW_VERSION={{TAG}} bash
```

Make sure [tmux](https://github.com/tmux/tmux/wiki/Installing) is installed on
your machine and on every machine you connect to.

For Android, install `muxflow-{{VERSION}}-android.apk` below.
