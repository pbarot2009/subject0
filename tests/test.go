// Go Syntax Test
package main

import "fmt"

type Client struct {
	Host string
	Port int
}

func (c *Client) Connect() error {
	if c.Port <= 0 {
		return fmt.Errorf("invalid port %d", c.Port)
	}
	return nil
}
