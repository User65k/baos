#!/usr/bin/python3.9
# -*- coding: utf-8 -*-
#
# Open all blinds now
# dont if its sunny and hot

from socket import socket
from jwetter import Weather
from jsonne import secs_till, rollo_mitte_blocked, w4_schatten, balkon_schatten
from astral import SunDirection
from sys import argv
from os.path import exists

if argv[-1]=="t":
  def sleep(x):
    print(f"sleep {x}")
    input()
else:
  from time import sleep

def main():
  if not exists("/home/pi/urlaub"):
    send(b"B A")
    #send(b"K 0")
  if weather():
    send(b"W A")
  else:
   send(b"W3 A")
   keep_sun_out()

def keep_sun_out():
  send(b"W1 Z")
  send(b"W2 Z")
  send(b"W4 Z")
  sleep(70)
  for _ in range(3):
    send(b"W4 U")
  for _ in range(3):#7
    send(b"W1 U")
  for _ in range(3):#7
    send(b"W2 U")
  w = secs_till(rollo_mitte_blocked, SunDirection.RISING)
  if w > 0:
    sleep(w)
  send(b"W2 U")
  send(b"W1 U")
  send(b"W4 U")
  w = secs_till(w4_schatten, SunDirection.RISING)
  if w > 0:
    sleep(w)
  send(b"W4 A")
  try:
   w = secs_till(balkon_schatten, SunDirection.RISING)
  except ValueError:
    # sun never reiches x (August)
    w = 0
  if w > 0:
    sleep(w)
  send(b"W1 A")
  send(b"W2 A")

def send(val):
    s = socket()
    s.connect(("127.0.0.1", 1337))
    s.sendall(val)
    
def wetter_match(alt):
  return alt.startswith("Clear") \
      or alt.startswith("Partly cloudy") \
      or alt.startswith("Mixed")
 
def weather():
  try:
    w = Weather()
    if wetter_match(w.pic(6)) \
        and wetter_match(w.pic(9)) \
        and w.curr_temp() > 10 \
        and w.max_temp() > 20:
          return False
  except Exception as e:
    print(e)
  return True

if __name__ == '__main__':
  main()
