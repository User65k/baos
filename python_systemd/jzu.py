#!/usr/bin/python3.9
# -*- coding: utf-8 -*-
#
# Close all blinds on dusk

from time import sleep
from socket import socket
from jsonne import og2, now
from astral.sun import dusk
from os.path import exists

n = now()
dep = 3.0 # sommer es bleibt hell
if n.month > 10 or n.month < 3:
 dep = 0.5

d=(dusk(og2, depression=dep)-n).total_seconds()
if d>0:
  sleep(d)
  print("Es wird dunkel")

def main():
  if exists("/home/pi/urlaub"):
    send(b"A Z")
    return
  send(b"W Z")
  send(b"B Z")
  sleep(70)
  send(b"W A")
  send(b"B A")
  sleep(2)
  send(b"W S")
  send(b"B S")

def send(val):
    s = socket()
    s.connect(("127.0.0.1", 1337))
    s.sendall(val)

if __name__ == '__main__':
    main()
