#!/usr/bin/python3.9
# -*- coding: utf-8 -*-

#
# Copyright (c) 2002-2022 WEINZIERL ENGINEERING GmbH
# All rights reserved.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE, TITLE AND NON-INFRINGEMENT. IN NO EVENT
# SHALL THE COPYRIGHT HOLDERS BE LIABLE FOR ANY DAMAGES OR OTHER LIABILITY,
# WHETHER IN CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
# WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE
#

import time
from ctypes import (
    CDLL, CFUNCTYPE,
    create_string_buffer,
    c_int, c_void_p,
    c_ushort, c_uint, c_ubyte,
    byref
)


# load the kdriveExpress dll (windows)
# for linux replace with kdriveExpress.so
kdrive = CDLL('./libkdriveExpress.so')

# the error callback pointer to function type
ERROR_CALLBACK = CFUNCTYPE(None, c_int, c_void_p)

# defines from kdrive (not available from the library)
KDRIVE_INVALID_DESCRIPTOR = -1
KDRIVE_ERROR_NONE = 0
KDRIVE_LOGGER_FATAL = 1
KDRIVE_LOGGER_INFORMATION = 6
SERIAL_NUMBER_LENGTH = 6

# The maximum number of individual addresses we expect to read at one time
MAX_IND_ADDR = 5

# Set to 1 to use connection-oriented
# Set to 0 for connection-less
connection_oriented = 0

# The address of the device that we connect
# to for the device services (Property Value Read etc)
address = c_ushort(0x1165)


def main():
    # Configure the logging level and console logger
    kdrive.kdrive_logger_set_level(KDRIVE_LOGGER_INFORMATION)
    kdrive.kdrive_logger_console()

    # We register an error callback as a convenience logger function to
    # print out the error message when an error occurs.
    error_callback = ERROR_CALLBACK(on_error_callback)
    kdrive.kdrive_register_error_callback(error_callback, None)

    # We create a Access Port descriptor. This descriptor is then used for
    # all calls to that specific access port.
    ap = open_access_port()

    # We check that we were able to allocate a new descriptor
    # This should always happen, unless a bad_alloc exception is internally thrown
    # which means the memory couldn't be allocated, or there are no usb ports available
    # or we were otherwise unable to open the port
    if ap == KDRIVE_INVALID_DESCRIPTOR:
        kdrive.kdrive_logger(KDRIVE_LOGGER_FATAL, 'Unable to create access port. This is a terminal failure'.encode('ascii'))
        while 1:
            pass
    kdrive.kdrive_logger(KDRIVE_LOGGER_INFORMATION, 'Send group value write')

    send(kdrive, ap, 0x10aa, 0) # fully open
    # close the access port
    kdrive.kdrive_ap_close(ap)

    # releases the access port
    kdrive.kdrive_ap_release(ap)

def send(kdrive, ap, addr, val):
    buf = (c_ubyte * 1)(val)
    error = kdrive.kdrive_ap_group_write(ap, addr, buf, 1)

def open_access_port():
    ap = kdrive.kdrive_ap_create()
    if (ap != KDRIVE_INVALID_DESCRIPTOR):
        if kdrive.kdrive_ap_open_serial_ft12(ap, '/dev/ttyAMA0'.encode('ascii')) != KDRIVE_ERROR_NONE:
            kdrive.kdrive_ap_release(ap)
            ap = KDRIVE_INVALID_DESCRIPTOR
    return ap


def on_error_callback(e, user_data):
    len = 1024
    str = create_string_buffer(len)
    kdrive.kdrive_get_error_message(e, str, len)
    print('kdrive error {0} {1}'.format(hex(e), str.value))


if __name__ == '__main__':
    main()
