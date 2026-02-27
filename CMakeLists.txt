cmake_minimum_required(VERSION 3.10)

# Set the project name
project(Fidan)

# Include the FetchContent module
include(FetchContent)

# Specify the C++ standard and compiler flags
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED True)

# Fetch the external argparse repository
FetchContent_Declare(
  argparse
  GIT_REPOSITORY https://github.com/p-ranav/argparse.git
  GIT_TAG        master  # You can specify a specific tag or branch if needed
)

# Make the content available after setting C++ standard and flags
FetchContent_MakeAvailable(argparse)

# Set default build type if not specified
if(NOT CMAKE_BUILD_TYPE OR CMAKE_BUILD_TYPE STREQUAL "")
  set(CMAKE_BUILD_TYPE "Release" CACHE STRING "Choose the type of build." FORCE)
endif()

# Specify the source files and include directories
file(GLOB_RECURSE SOURCES "fidan/src/*.cpp")
include_directories("fidan/src/headers")

# Include the argparse directory
include_directories(${argparse_SOURCE_DIR}/include)

# Create the executable target
add_executable(fidan ${SOURCES})

# Set compiler flags based on the compiler
if(MSVC)
    # MSVC flags for Release configuration
    target_compile_options(fidan PRIVATE $<$<CONFIG:Release>:/O2 /GL /fp:fast /arch:AVX2>)
    set(CMAKE_EXE_LINKER_FLAGS_RELEASE "${CMAKE_EXE_LINKER_FLAGS_RELEASE} /LTCG")
    
    # MSVC flags for Debug configuration
    target_compile_options(fidan PRIVATE $<$<CONFIG:Debug>:/Zi /Od>)
else()
    # GCC/Clang flags for Release configuration
    target_compile_options(fidan PRIVATE $<$<CONFIG:Release>:-O3 -march=native -flto=4 -funroll-loops -finline-functions -fomit-frame-pointer -fuse-linker-plugin>)
    
    # GCC/Clang flags for Debug configuration
    target_compile_options(fidan PRIVATE $<$<CONFIG:Debug>:-g>)
endif()

# Add a definition for Debug mode using target_compile_definitions
target_compile_definitions(fidan PRIVATE $<$<CONFIG:Debug>:DEBUG_MODE>)

# Set the output directory for the executable
set_target_properties(fidan PROPERTIES
    RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/bin"
    RUNTIME_OUTPUT_DIRECTORY_RELEASE "${CMAKE_CURRENT_SOURCE_DIR}/bin"
    RUNTIME_OUTPUT_DIRECTORY_DEBUG "${CMAKE_CURRENT_SOURCE_DIR}/bin"
)

# Install the executable
install(TARGETS fidan
    RUNTIME DESTINATION bin
)

# Install the builtins
install(DIRECTORY fidan/builtins/
    DESTINATION include/builtins
    FILES_MATCHING PATTERN "*.h" PATTERN "*.fdn"
)

# Install other files (e.g. README.md, LICENSE.md)
install(FILES README.md LICENSE.md
    DESTINATION .
)