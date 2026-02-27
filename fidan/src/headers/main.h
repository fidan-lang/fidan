// Copyright (c) Kaan Gönüldinc (AppSolves). All rights reserved.
// See LICENSE file in the project root for full license information.

#ifndef FIDAN_MAIN_H
#define FIDAN_MAIN_H

// Include necessary headers
#include <string>
#include <iostream>
#include <unordered_map>

// Constants
const std::string version = "1.0.0";
const std::string author = "AppSolves (Kaan Gönüldinc)";
const std::string copyRight = "Copyright (C) 2024 " + author + ". ALL RIGHTS RESERVED.";
const std::string license = "\nNON PRODUCTION\n\nBy now, Fidan is not ready for production. It is still in development.\n" + copyRight;
const std::string credits = "\nThanks to " + author + " and all contributors for supporting the development of Fidan!\n";
const std::string help = R""""(
Fidan is a programming language that is designed to be simple and easy to use.
It is still in development and not ready for production.
)"""";

// Map of commands
const std::unordered_map<std::string, void (*)()> commands = {
    {"help", []()
     { print(help); }},
    {"license", []()
     { print(license); }},
    {"credits", []()
     { print(credits); }},
    {"clear", []()
     {
         int result = system("clear");
         if (result != 0)
         {
             std::cerr << "Failed to clear the screen" << std::endl;
         }
     }}}; // clearScreen

#endif // FIDAN_MAIN_H
