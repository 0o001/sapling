#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import abc
import argparse
from typing import Callable, List, Optional, Type

from . import util


class Subcmd(abc.ABC):
    NAME: Optional[str] = None
    HELP: Optional[str] = None

    def __init__(self, parser: argparse.ArgumentParser) -> None:
        # Save a pointer to the parent ArgumentParser that this Subcmd belongs
        # to.  This is primarily useful for HelpCmd so it can show help
        # information about its sibling commands.
        self.parent_parser = parser

    def add_parser(self, subparsers: argparse._SubParsersAction) -> None:
        # If get_help() returns None, do not pass in a help argument at all.
        # This will prevent the command from appearing in the help output at
        # all.
        kwargs = {}
        help = self.get_help()
        if help is not None:
            kwargs['help'] = help
        parser = subparsers.add_parser(self.get_name(), **kwargs)
        parser.set_defaults(func=self.run)
        self.setup_parser(parser)

    def get_name(self) -> str:
        if self.NAME is None:
            raise NotImplementedError('Subcmd subclasses must set NAME')
        return self.NAME

    def get_help(self) -> Optional[str]:
        return self.HELP

    def setup_parser(self, parser: argparse.ArgumentParser) -> None:
        # Subclasses should override setup_parser() if they have any
        # command line options or arguments.
        pass

    def add_subcommands(
        self, parser: argparse.ArgumentParser, cmds: List[Type['Subcmd']]
    ) -> argparse._SubParsersAction:
        return add_subcommands(parser, cmds)

    @abc.abstractmethod
    def run(self, args: argparse.Namespace) -> int:
        pass


CmdTable = List[Type[Subcmd]]


def subcmd(
    name: str,
    help: Optional[str] = None,
    cmd_table: Optional[CmdTable] = None
) -> Callable[[Type[Subcmd]], Type[Subcmd]]:
    '''
    @subcmd() is a decorator that can be used to help define Subcmd instances

    If the cmd_table argument is non-None the new Subcmd class will
    automatically be added to this list.

    Example usage:

        @subcmd('list', 'Show the result list')
        class ListCmd(Subcmd):
            def run(self, args: argparse.Namespace) -> int:
                # Perform the command actions here...
                pass
    '''
    def wrapper(cls: Type[Subcmd]) -> Type[Subcmd]:
        class SubclassedCmd(cls):
            NAME = name
            HELP = help

        if cmd_table is not None:
            cmd_table.append(SubclassedCmd)
        return SubclassedCmd

    return wrapper


class Decorator(object):
    '''
    decorator() creates a new object that can act as a decorator function to
    help define Subcmd instances.

    This decorator object also maintains a list of all commands that have been
    defined using it.  This command list can later be passed to
    add_subcommands() to register these commands.
    '''
    def __init__(self) -> None:
        self.commands: CmdTable = []

    def __call__(self, name: str,
                 help: str) -> Callable[[Type[Subcmd]], Type[Subcmd]]:
        return subcmd(name, help, cmd_table=self.commands)


def add_subcommands(parser: argparse.ArgumentParser,
                   cmds: List[Type[Subcmd]]) -> argparse._SubParsersAction:
    # Sort the commands alphabetically.
    # The order they are added here is the order they will be displayed
    # in the --help output.
    subparsers = parser.add_subparsers()
    for cmd_class in sorted(cmds, key=lambda c: c.NAME):
        cmd_instance = cmd_class(parser)
        cmd_instance.add_parser(subparsers)

    return subparsers


def _get_subparsers(
    parser: argparse.ArgumentParser
) -> Optional[argparse._SubParsersAction]:
    if parser._subparsers is None:
        return None

    for action in parser._subparsers._actions:
        if action.option_strings:
            continue
        if not isinstance(action, argparse._SubParsersAction):
            # This is not a subcommand
            return None
        return action

    return None


def do_help(
    parser: argparse.ArgumentParser,
    help_args: List[str],
) -> int:
    # Figure out what subcommand we have been asked to show the help for.
    for idx, arg in enumerate(help_args):
        subcmds = _get_subparsers(parser)
        if subcmds is None:
            cmd_so_far = ' '.join(help_args[:idx])
            # The remaining arguments may be positional arguments
            # to this command.  Stop processing arguments here and show help
            # for the command we have processed so far.
            break

        subparser = subcmds.choices.get(arg, None)
        if not subparser:
            if idx == 0:
                util.print_stderr('error: unknown command "{}"', arg)
            else:
                cmd_so_far = ' '.join(help_args[:idx])
                util.print_stderr('error: "{}" does not have a subcommand "{}"',
                                  cmd_so_far, arg)
            return 2

        parser = subparser

    parser.print_help()
    return 0


@subcmd('help', 'Display command line usage information')
class HelpCmd(Subcmd):
    def setup_parser(self, parser: argparse.ArgumentParser) -> None:
        parser.add_argument('args', nargs='*')

    def run(self, args: argparse.Namespace) -> int:
        return do_help(self.parent_parser, args.args)
