package main

import "net/http"

func main() {
	http.ListenAndServe(":8080", http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { w.Write([]byte("ok")) }))
}
