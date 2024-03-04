#!/usr/bin/python3.9
# -*- coding: utf-8 -*-
#
# Close the blinds in step with the setting sun
from sys import exit
from jwetter import Weather
from jsonne import secs_till, buero_bekommt_sonne, rollo_mitte_blocked, rollo_1tick, rollo_2tick, rollo_3tick
from astral import SunDirection
from time import sleep
from socket import socket
from os.path import exists

def send(val):
    s = socket()
    s.connect(("127.0.0.1", 1337))
    s.sendall(val)

def wetter_match(alt):
  return alt.startswith("Clear") \
      or alt.startswith("Partly cloudy") \
      or (alt.startswith("Mixed") \
          and alt.endswith("possible"))

w = Weather()
if wetter_match(w.pic(12)) \
  and wetter_match(w.pic(15)) \
  and w.curr_temp() > 20 \
  and w.max_temp() > 20:
    print("mach schatten!")
else:
    print(f"{w.curr_temp()}/{w.max_temp()}, 12: {w.pic(12)}, 15: {w.pic(15)}")
    exit(0)

w = secs_till(buero_bekommt_sonne, SunDirection.SETTING)
if w > 0:
    sleep(w)
    print("buero bekommt sonne")

send(b"B Z")
send(b"S Z")
if exists("/home/pi/urlaub"):
  send(b"K Z")
  exit(0)
#send(b"K Z")
sleep(70)
for _ in range(4):
    send(b"B U")
    send(b"S U")
    #send(b"K U")

for winkel in (rollo_mitte_blocked, rollo_1tick, rollo_2tick, rollo_3tick):
 w = secs_till(winkel, SunDirection.SETTING)
 if w > 0:
    sleep(w)
 send(b"B D")
 send(b"S D")
 #send(b"K D")
