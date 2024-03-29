This is using a [kBerry](https://github.com/yene/kBerry) to control this

![jal-0810.02](./jal-0810.02.jpeg)

via KNX. It is in turn controlling my Venetian blinds.

# KNX

The device is listening to `group_write`.
The value written is either `0x0` ascend or `0x1` descend.

The circuits `A` to `H` mapped to an contiguous address range on the bus.
This address is added to `0x1100` to move a single step and to `0x1000` to move all the way.

Group ID `0` is the wind sensor. If it transmits `1` everything goes all the way up. Otherwise it transmits `0` and everything stays wherever it is.

# compile
drop libkdriveExpress.so in the repos root and run `cargo build --release` (on a Pi).
I think I got the library from [rpi-kdriveexpress-monitor](https://github.com/marssys/rpi-kdriveexpress-monitor)

```
md5sum baos_ctrl/libkdriveExpress.so
5e47f74ec10b8e35e4a852bc99b77674  baos_ctrl/libkdriveExpress.so
```

# run

The program will listen on port 1337 for TCP connections.
You can send it `T C` where `T` is the target (or a Group like `A` for all) and `C` is the command:

| C | Action    |
|---|-----------|
| 1 | Close     |
| 0 | Open      |
| Z | Close     |
| A | Open      |
| S | Stop      |
| D | Step Down |
| U | Step Up   |

Stop equals Step Down like on the included remote

# Python & systemd controller

`./python_systemd/` contains some python files (that I have in my `/home/pi/` folder) and some systemd timers and units (from `/etc/systemd/system/`) to trigger those at sundown (`j_zu`), 8am (`j_auf`) and during the summer when the sun is shining in (`j_schatten`).

Astronomic calculations are done via `astral`

