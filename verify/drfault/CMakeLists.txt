cmake_minimum_required(VERSION 3.20)
# CXX 도 켠다: SDK 설정 파일이 CMAKE_CXX_SIZEOF_DATA_PTR 를 본다
project(drfault C CXX)

# DynamoRIO SDK 는 -DDynamoRIO_DIR=<sdk>/cmake 로 넘긴다 (저장소에 넣지 않음)
find_package(DynamoRIO REQUIRED)

add_library(drfault SHARED drfault.c)
configure_DynamoRIO_client(drfault)
use_DynamoRIO_extension(drfault drmgr)
