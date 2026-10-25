PREFIX   ?= $(HOME)/.local
PKGS     := x11 xext xrandr cairo pangocairo libqalculate
CXX      ?= g++
CC       ?= cc
OPT      ?= -O2 -flto
DEP_CFLAGS ?= $(shell pkg-config --cflags $(PKGS))
DEP_LIBS   ?= $(shell pkg-config --libs $(PKGS))
CXXFLAGS += -std=c++17 $(OPT) -Wall -Wextra -Wno-unused-parameter $(DEP_CFLAGS)
CFLAGS   += -std=c11 $(OPT) -Wall -Wextra -D_GNU_SOURCE
LDFLAGS  += $(OPT) -Wl,--as-needed -Wl,-O1

DAEMON_SRC := src/main.cpp src/engine.cpp src/store.cpp
DAEMON_OBJ := $(DAEMON_SRC:src/%.cpp=build/%.o)

all: build/qcalc build/qcalcd

build/qcalc: src/client.c src/common.h | build
	$(CC) $(CFLAGS) $(LDFLAGS) -s src/client.c -o $@

build/qcalcd: $(DAEMON_OBJ)
	$(CXX) $(LDFLAGS) -s $(DAEMON_OBJ) -o $@ $(DEP_LIBS) -lpthread

build/%.o: src/%.cpp src/*.h | build
	$(CXX) $(CXXFLAGS) -c $< -o $@

build:
	mkdir -p build

install: all
	install -Dm755 build/qcalc $(DESTDIR)$(PREFIX)/bin/qcalc
	install -Dm755 build/qcalcd $(DESTDIR)$(PREFIX)/bin/qcalcd

clean:
	rm -rf build

.PHONY: all install clean
