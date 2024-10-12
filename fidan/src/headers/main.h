// Copyright (c) AppSolves (Kaan Gönüldinc). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_MAIN_H
#define FIDAN_MAIN_H

#include <string>
#include <iostream>
#include <unordered_map>

const std::string version = "1.0.0";
const std::string author = "AppSolves (Kaan Gönüldinc)";
const std::string copyRight = "Copyright (C) 2024 " + author + ". ALL RIGHTS RESERVED.";
const std::string license = "\nNON PRODUCTION\n\nBy now, Fidan is not ready for production. It is still in development.\n" + copyRight;
const std::string credits = "\nThanks to " + author + " and all contributors for supporting the development of Fidan!\n";
const std::string help = R""""(
Fidan is a programming language that is designed to be simple and easy to use.
It is still in development and not ready for production.
)"""";

const void print(const std::string &text)
{
    std::cout << text << std::endl;
}

const std::unordered_map<std::string, void (*)()> commands = {
    {"help", []()
     { print(help); }},
    {"license", []()
     { print(license); }},
    {"credits", []()
     { print(credits); }},
    {"clear", []()
     { system("clear"); }}}; // clearScreen

#endif // FIDAN_MAIN_H
