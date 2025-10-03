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
if n.month == 3:
  dep = (2.5*n.day/31)+0.5
  #n.day..31 -> 0.5..3
if n.month == 9:
  dep = (2.5*(30-n.day)/30)+0.5

d=(dusk(og2, depression=dep)-n).total_seconds()
if d>0:
  sleep(d)
  print("Es wird dunkel")

def main():
  if exists("/home/pi/urlaub"):
    send(b"A Z")
    return
  send(b"W \x80\a")
  send(b"B \x80\a")

def send(val):
    s = socket()
    s.connect(("127.0.0.1", 1337))
    s.sendall(val)

if __name__ == '__main__':
    main()
