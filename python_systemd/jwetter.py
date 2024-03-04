#!/usr/bin/python3.9
# -*- coding: utf-8 -*-

from requests import Session
#from xml.etree.ElementTree import fromstring, canonicalize
from lxml.html import fromstring

class Weather():
 def __init__(self):
  with Session() as sess:
    sess.headers.update({
      'User-Agent': 'Mozilla/5.0 (X11; Fedora; Linux x86_64; rv:91.0) Gecko/20100101 Firefox/91.0',
      'Accept': 'text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8',
      'Accept-Language': 'en-US,en;q=0.5',
      'Connection': 'keep-alive',
    })
    weather = sess.get('https://www.meteoblue.com/en/weather/widget/three/neuaubing_germany_2866274',
                params={'geoloc':'fixed', 'nocurrent':'0', 
                        'noforecast':'0', 'days':'4', 
                        'tempunit':'CELSIUS', 
                        'windunit':'KILOMETER_PER_HOUR', 
                        'layout':'bright'})
    #...</header>...<footer>... -> /html/body/div[2]/
    t = weather.text
    h = t.find("</header>")
    if h > -1:
      h += 9
      f = t.find("<footer>", h)
      if f > -1:
        #with open('test','w') as tf:
        #  tf.write(t[h:f])
        #t = canonicalize("<div>" + t[h:f].replace('<br>','') + "</div>", 
        #    strip_text=True)
        t = t[h:f]
    self.root = fromstring(t)
 def find(self, f):
   return self.root.find(f)
 def curr_temp(self):
    t = int(self.find('div/div[1]/div[1]/span[1]').text.strip())
    #print(f"Curr Temp: {t}")
    return t
 def max_temp(self):
    t = int(self.root.find('main/div[1]/ul/li[1]/div[2]/div[1]').text.strip()[:-3])
    #print(f"Max Temp: {t}")
    return t
 def pic_day(self):
    pic = self.root.find('main/div[1]/ul/li[1]/div[1]/div/img').attrib['alt'] 
    #print(f"today {pic}")
    return pic
    #oder alt starswith "Clear"
 def pic(self, h):
   """
   6:00: 2
   9:00: 3
   """
   no = int(h/3)
   pic = self.root.find('main/div[2]/ul/li[1]/table/tbody/tr[2]/td['+str(no)+']/div/div/img').attrib['alt']
   #print(f"{h}:00 {pic}")
   return pic
 def temp(self, h):
   """
   6:00: 2
   9:00: 3
   """
   no = int(h/3)
   return int(self.root.find('main/div[2]/ul/li[1]/table/tbody/tr[3]/td['+str(no)+']/div').text[:-1])

if __name__ == '__main__':
    w = Weather()
    print(f"Curr Temp: {w.curr_temp()}°C") # > 10
    print(f"Max Temp: {w.max_temp()}°C") # > 20
    print("Tag: "+w.pic_day())
    for h in range(3,25,3):
      print(f"{h}Uhr: {w.temp(h)}°C - "+w.pic(h))
    if w.pic_day().startswith("Clear") \
        and w.pic(6).startswith("Clear") \
        and w.pic(9).startswith("Clear") \
        and w.curr_temp() > 10 \
        and w.max_temp() > 20:
          print("nicht hoch")
