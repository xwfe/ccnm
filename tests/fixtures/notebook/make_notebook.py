"""Write tests/fixtures/notebook/analysis.ipynb the way nbformat does:
json.dumps(sort_keys=True, indent=1, ensure_ascii=False) plus a final newline."""

import base64
import json
import struct
import sys
import zlib

ESC = chr(27)


def png_2x2():
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
    raw = b"".join(b"\x00" + b"\xff\x00\x00" * 2 for _ in range(2))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


b64 = base64.b64encode(png_2x2()).decode()
red = ESC + "[0;31m"
green = ESC + "[0;32m"
reset = ESC + "[0m"
nb = {
    "cells": [
        {"cell_type": "markdown", "id": "5a1c0e2f", "metadata": {},
         "source": ["# 销售分析\n", "\n", "Load the data and plot it."]},
        {"cell_type": "code", "execution_count": 1, "id": "b7d3a901", "metadata": {},
         "outputs": [{"name": "stdout", "output_type": "stream", "text": ["rows: 3\n", "columns: 2\n"]}],
         "source": ["import pandas as pd\n", "df = pd.read_csv(\"sales.csv\")\n",
                    "print(\"rows:\", len(df))\n", "print(\"columns:\", len(df.columns))"]},
        {"cell_type": "code", "execution_count": 2, "id": "c4e8f7aa", "metadata": {"tags": ["plot"]},
         "outputs": [
             {"data": {"image/png": b64 + "\n", "text/plain": ["<Figure size 640x480 with 1 Axes>"]},
              "metadata": {}, "output_type": "display_data"},
             {"data": {"text/plain": ["42"]}, "execution_count": 2, "metadata": {},
              "output_type": "execute_result"}],
         "source": ["df.plot()\n", "42"]},
        {"cell_type": "code", "execution_count": 3, "id": "d0f19b3c", "metadata": {},
         "outputs": [{"ename": "ZeroDivisionError", "evalue": "division by zero", "output_type": "error",
                      "traceback": [
                          red + "-" * 40 + reset,
                          red + "ZeroDivisionError" + reset + "   Traceback (most recent call last)",
                          "Cell " + green + "In[3], line 1" + reset + "\n----> 1 1/0\n",
                          red + "ZeroDivisionError" + reset + ": division by zero"]}],
         "source": ["1/0"]},
        {"cell_type": "code", "execution_count": None, "id": "e92b4d10", "metadata": {}, "outputs": [],
         "source": []},
    ],
    "metadata": {
        "kernelspec": {"display_name": "Python 3 (ipykernel)", "language": "python", "name": "python3"},
        "language_info": {"codemirror_mode": {"name": "ipython", "version": 3}, "file_extension": ".py",
                          "mimetype": "text/x-python", "name": "python", "nbconvert_exporter": "python",
                          "pygments_lexer": "ipython3", "version": "3.12.4"},
    },
    "nbformat": 4,
    "nbformat_minor": 5,
}
text = json.dumps(nb, sort_keys=True, indent=1, ensure_ascii=False)
if not text.endswith("\n"):
    text += "\n"
with open(sys.argv[1], "w", encoding="utf-8") as f:
    f.write(text)
print(len(text.encode()))
