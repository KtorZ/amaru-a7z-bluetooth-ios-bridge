# Amaru on Radxa A7z

## Hardware

- 1x Radxa Cubie a7z 2GB+
- 1x MicroSD Samsung Pro Plus (microSDXC, U3, UHS-I)
- 1x Radxa UFS module
- 1x Flirc Raspberry Pi Zero Case
- (optional) 1x Anker Nano 5000 mAh, 18 W, 18.50 Wh

## Flashing The MicroSD

### Download & flash image

- Use: [radxa-a733_bullseye_cli_r6.output_512.img.xz](https://github.com/radxa-build/radxa-a733/releases/tag/rsdk-r6) (/!\ 512, not 4096)
- Pi imager: https://www.raspberrypi.com/software/

### Configure Wi-Fi

Find the right disk with the SD card:

```console
$ diskutil list
/dev/disk10 (internal, physical):
   #:                       TYPE NAME                    SIZE       IDENTIFIER
   0:      GUID_partition_scheme                        *512.7 GB   disk10
   1:           Linux Filesystem                         16.8 MB    disk10s1
   2:                        EFI efi                     314.6 MB   disk10s2
   3:           Linux Filesystem                         3.4 GB     disk10s3
                    (free space)                         508.9 GB   -
```

Mount the ~16.8MB Linux Filesystem:

```console
sudo mkdir -p /Volumes/config
sudo mount -t msdos /dev/disk10s1 /Volumes/config
```

Add the configuration

```console
cat >> /Volumes/config/before.txt <<'EOF'
connect_wi-fi WIFI_SSID WIFI_PASSWORD
enable_service ssh
EOF
```

Unmount and eject the disk:

```console
sudo umount /Volumes/config
diskutil eject /dev/disk10
```

Then turn on the device and login via ssh using mDNS and the [following credentials](https://docs.radxa.com/en/cubie/a7z/getting-started/quickly-start#system-information):

| username | radxa |
| ---      | ---   |
| password | radxa |

```console
ssh radxa-a733.local
```

## Flashing The UFS Module

Follow official instructions at <https://docs.radxa.com/en/cubie/a7z/getting-started/install-system/ufs>.

Find the right disk:

```console
$ lsblk
NAME          SIZE TYPE FSTYPE
sda         238.4G disk         <--------- UFS Module
├─sda1        128M part vfat
├─sda2        2.3G part vfat
└─sda3        6.4G part ext4
mmcblk0     477.5G disk         <--------- SD Card
├─mmcblk0p1    16M part vfat
├─mmcblk0p2   300M part vfat
└─mmcblk0p3 477.2G part ext4
zram0         3.9G disk

```

Download the 4096 image:

```console
sudo apt update
sudo apt install -y wget xz-utils
cd /var/tmp
wget -c https://github.com/radxa-build/radxa-a733/releases/download/rsdk-r6/radxa-a733_bullseye_cli_r6.output_4096.img.xz
```

Flash the UFS module:

```console
xz -dc radxa-a733_bullseye_cli_r6.output_4096.img.xz | sudo dd of=/dev/sda bs=4M conv=fsync status=progress
sync
sudo partprobe /dev/sda
```

Mount the configuration volume and re-configure the wi-fi:

```console
sudo mkdir -p /mnt/ufs-config
sudo mount /dev/sda1 /mnt/ufs-config
sudo tee -a /mnt/ufs-config/before.txt >/dev/null <<'EOF'
connect_wi-fi WIFI_SSID WIFI_PASSWORD
enable_service ssh
EOF
sudo umount /mnt/ufs-config
sync
```

Shutdown the device:

```console
sudo poweroff
```

Then REMOVE THE SD CARD, put power back on and login again:

```
ssh radxa-a733.local
```

> [!CAUTION]
>
> If you chosed to add the fingerprint to the SSH known_hosts previously, this
> will now likely not match. Remove the entry from the known_hosts if that's
> the case.

## Restoring the SD as storage

(not tested)

Insert the SD card, and mount it as a permanent data storage:

```console
sudo mkdir -p /srv/sdcard

UUID="$(sudo blkid -s UUID -o value /dev/mmcblk0p3)"
printf 'UUID=%s /srv/sdcard ext4 defaults,nofail,x-systemd.device-timeout=10s 0 2\n' "$UUID" \
  | sudo tee -a /etc/fstab

sudo mount -a
findmnt /srv/sdcard
```

## Installing & bootstrapping Amaru

```console
wget https://nightly.amaru.global/downloads/linux-aarch64.deb
sudo apt install ./linux-aarch64.deb

sudo install -d -o amaru -g amaru -m 0750 \
  /var/lib/amaru \
  /var/lib/amaru/chain.mainnet.db \
  /var/lib/amaru/ledger.mainnet.db

# TODO: replace the initial configuration in the debian archive
sudo tee /etc/default/amaru >/dev/null <<'EOF'
AMARU_NETWORK=mainnet
AMARU_CHAIN_DIR=/var/lib/amaru/chain.mainnet.db
AMARU_LEDGER_DIR=/var/lib/amaru/ledger.mainnet.db
AMARU_MIGRATE_CHAIN_DB=true
AMARU_PID_FILE=/run/amaru/amaru.pid
EOF

sudo systemd-run \
  --unit=amaru-bootstrap \
  --collect \
  --wait \
  --service-type=exec \
  --uid=amaru \
  --gid=amaru \
  --working-directory=/var/lib/amaru \
  /usr/bin/env \
  AMARU_NETWORK=mainnet \
  AMARU_CHAIN_DIR=/var/lib/amaru/chain.mainnet.db \
  AMARU_LEDGER_DIR=/var/lib/amaru/ledger.mainnet.db \
  AMARU_MIGRATE_CHAIN_DB=true \
  /usr/bin/amaru node bootstrap --network mainnet

sudo systemctl enable --now amaru.service
sudo systemctl status amaru.service
```
