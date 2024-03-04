#!/usr/bin/python3.9
# -*- coding: utf-8 -*-
from astral import Observer, sun, SunDirection
from zoneinfo import ZoneInfo
from datetime import datetime
from suntime import Sun, SunTimeException

latitude = 48.139768
longitude = 11.419107

og2 = Observer(48.1378537,11.4173478)

rollo_mitte_blocked = 36.0
rollo_1tick = 30.4
rollo_2tick = 25.507
rollo_3tick = 14.74
w4_schatten = 52
balkon_schatten = 54.7 #62
buero_bekommt_sonne = 51.9
#azimuth >238 es scheint zum büro rein
#az >179 balkon clear

def now():
    return datetime.now(tz=ZoneInfo('Europe/Berlin'))
    
def secs_till(w,d):
  global og2
  try:
    return (sun.time_at_elevation(og2, w, direction = d)-now()).total_seconds()
  except ValueError:
    return (sun.noon(og2)-now()).total_seconds()

if __name__ == '__main__':
  print(now())
  print(f"az {sun.azimuth(og2)}")
  print(f"el {sun.elevation(og2)}")
  print(f"rollo_mitte_blocked\t{sun.time_at_elevation(og2, rollo_mitte_blocked, direction = SunDirection.RISING):%H:%M:%S}")
  try:
   print(f"w4_schatten\t{sun.time_at_elevation(og2, w4_schatten, direction = SunDirection.RISING):%H:%M:%S}")
  except ValueError:
    pass
  try:
   print(f"balkon_schatten\t{sun.time_at_elevation(og2, balkon_schatten, direction = SunDirection.RISING):%H:%M:%S}")
  except ValueError:
    pass
  print(f"noon\t{sun.noon(og2):%H:%M:%S}")
  try:
   print(f"buero_bekommt_sonne\t{sun.time_at_elevation(og2, buero_bekommt_sonne, direction = SunDirection.SETTING):%H:%M:%S}")
  except ValueError:
    pass
  print(f"rollo_mitte_blocked\t{sun.time_at_elevation(og2, rollo_mitte_blocked, direction = SunDirection.SETTING):%H:%M:%S}")
  print(f"rollo_1tick\t{sun.time_at_elevation(og2, rollo_1tick, direction = SunDirection.SETTING):%H:%M:%S}")
  print(f"rollo_2tick\t{sun.time_at_elevation(og2, rollo_2tick, direction = SunDirection.SETTING):%H:%M:%S}")
  print(f"rollo_3tick\t{sun.time_at_elevation(og2, rollo_3tick, direction = SunDirection.SETTING):%H:%M:%S}")
  s=sun.sun(og2)
  print(f"set\t{s['sunset']:%H:%M:%S}")
  print(f"dusk3\t{sun.dusk(og2, depression=3.0):%H:%M:%S}")
  print(f"dusk\t{s['dusk']:%H:%M:%S}")
  sun2 = Sun(latitude, longitude)
  print(sun2.get_sunset_time())
