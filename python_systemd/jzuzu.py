#!/usr/bin/python3.9
# -*- coding: utf-8 -*-
#
# Close all blinds now

from socket import socket

def main():
    send(b"A 1")

def send(val):
    s = socket()
    s.connect(("127.0.0.1", 1337))
    s.sendall(val)

if __name__ == '__main__':
    main()
