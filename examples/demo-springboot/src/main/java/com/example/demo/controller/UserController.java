package com.example.demo.controller;

import com.example.demo.service.UserService;
import com.example.demo.model.User;
import org.springframework.web.bind.annotation.*;
import java.util.List;

@RestController
@RequestMapping("/api/users")
public class UserController {
    private final UserService userService;

    public UserController(UserService userService) {
        this.userService = userService;
    }

    @GetMapping("/list")
    public List<User> listUsers() {
        return userService.getAllUsers();
    }

    @PostMapping("/create")
    public void createUser(@RequestBody User user) {
        userService.saveUser(user);
    }
}
